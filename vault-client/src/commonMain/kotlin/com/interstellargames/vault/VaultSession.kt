package com.interstellargames.vault

import com.interstellargames.solana.ChainIds
import com.interstellargames.solana.SplIx
import com.interstellargames.solana.ZERO_KEY
import com.interstellargames.solana.client.AccountUpdate
import com.interstellargames.solana.client.ChainIo
import com.interstellargames.solana.client.KeepLoggedIn
import com.interstellargames.solana.client.Subscription
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.TimeSource
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * How far a funds flow has got. Moving money is several chain steps with real waits between them,
 * and a player watching a spinner deserves to know which one they are in. The steps are named
 * here; what to call them on screen is the client's.
 */
enum class FundsPhase {
    /** Committing the ledger back to the base chain, so it can be paid into. */
    ENDING_SESSION,

    /** The wallet has been asked to sign, and the transaction is going out. */
    APPROVING,

    /** Handing the ledger back to the rollup, where the games are played. */
    STARTING_SESSION,

    /** Paying the balance back out to the wallet. */
    WITHDRAWING,
}

/** A ledger as read — and whether the figure is live or the copy frozen at delegation. */
class LedgerRead(
    val ledger: LedgerState?,
    /** False when only the frozen as-of-delegation copy answered. */
    val live: Boolean,
    val delegated: Boolean,
    /** Why the read is not live, when it isn't — for the player, never swallowed. */
    val note: String? = null,
)

/**
 * Every sequence a client performs against the vault, once: funding, cashing out, starting and
 * ending the rollup session, and the session-store consent each game's receipts need. It runs on
 * [ChainIo], which only signs, sends and reads; which game is asking is passed in as a
 * [VaultGame], so one session serves every game a client offers.
 *
 * The rules live here so no client can be the one that forgot them: a delegated ledger comes home
 * before it is paid into or out of, and a key the store does not hold is granted on basenet —
 * riding the delegation when the ledger is home, on its own when it is already delegated.
 */
class VaultSession(val io: ChainIo) {

    private companion object {
        // MagicBlock's commit-chain log markers: the undelegate names the ScheduledCommitSent tx
        // ([SCHEDULED]); that tx names the base-chain signature it sent ([SCHEDULED_SENT]).
        const val SCHEDULED = "ScheduledCommitSent signature: "
        const val SCHEDULED_SENT = "ScheduledCommitSent signature[0]: "

        // The ceiling a player will actually sit through: past it they have started doing
        // something else or restarted the app, so a longer wait buys nothing.
        const val LEDGER_WAIT_MS = 30_000L
    }

    // ── reads ────────────────────────────────────────────────────────────────

    /** True once the ledger has been handed to the rollup — a session is live. */
    suspend fun isDelegated(owner: String): Boolean =
        io.basenetOwner(VaultPda.ledger(owner)) == ChainIds.DELEGATION

    /**
     * The player's ledger. A delegated ledger's truth is on the rollup; the basenet copy is frozen
     * at delegation, and saying so is better than quietly showing a stale figure.
     */
    suspend fun fetchLedger(owner: String): LedgerRead {
        val pda = VaultPda.ledger(owner)
        val base = runCatching { io.readBasenet(pda) }.getOrNull()
        if (base?.owner != ChainIds.DELEGATION) {
            return LedgerRead(base?.data?.let(LedgerCodec::decode), live = true, delegated = false)
        }
        return runCatching {
            LedgerRead(io.readRollup(pda)?.let(LedgerCodec::decode), live = true, delegated = true)
        }.getOrElse {
            LedgerRead(
                LedgerCodec.decode(base.data), live = false, delegated = true,
                note = it.message ?: "session read failed",
            )
        }
    }

    /** Calls [onLamports] whenever the wallet's own SOL moves. Always basenet: a wallet holds no
     *  data, so its update *is* the balance. */
    suspend fun watchWallet(owner: String, onLamports: (Long) -> Unit): Subscription? =
        io.watchBasenet(owner) { onLamports(it.lamports) }

    /**
     * Calls [onLedger] whenever the player's ledger moves, on whichever chain holds the live copy.
     *
     * Which chain that is is not a fact to settle once: every deposit hands the ledger to the
     * rollup and every withdrawal brings it home, so a subscription bound when the watch starts
     * spends the session on the wrong chain. Basenet always holds the account and its owner says
     * where the live copy is, so that subscription stays up and swaps the rollup one in beneath.
     */
    suspend fun watchLedger(owner: String, onLedger: (LedgerState) -> Unit): Subscription {
        val pda = VaultPda.ledger(owner)
        val scope = CoroutineScope(SupervisorJob())
        val rollupWatch = SwappableWatch(scope)
        // Closing a subscription takes a moment, so the rollup's last word can arrive after the
        // ledger came home — carrying the balance from before the withdrawal that brought it
        // home. Where the ledger lives decides whose updates count.
        var delegated = false
        val startRollup: suspend () -> Subscription? = {
            io.watchRollup(pda) { update ->
                if (delegated) update.data?.let(LedgerCodec::decode)?.let(onLedger)
            }
        }
        val base = runCatching {
            io.watchBasenet(pda) { update ->
                delegated = update.owner == ChainIds.DELEGATION
                // Delegated, the basenet copy is frozen as of delegation — never publish it.
                if (!delegated) update.data?.let(LedgerCodec::decode)?.let(onLedger)
                rollupWatch.want(delegated, startRollup)
            }
        }.getOrNull()
        delegated = runCatching { isDelegated(owner) }.getOrDefault(false)
        rollupWatch.want(delegated, startRollup)
        return object : Subscription {
            override suspend fun close() {
                scope.cancel()
                rollupWatch.close()
                runCatching { base?.close() }
            }
        }
    }

    // ── consent ──────────────────────────────────────────────────────────────

    /**
     * Makes sure the ledger is live in the rollup **and** the session store carries the consent
     * [game]'s receipts will need, returning the key that signs them. The store is never
     * delegated, so a missing key is granted on basenet whatever the ledger is doing: with the
     * delegation when the ledger is home, in a transaction of its own when it is not.
     */
    suspend fun ensureConsentedSession(owner: String, game: VaultGame): String {
        val signer = io.sessionKey(owner)
        val stale = staleAuthorization(owner, game)
        if (!isDelegated(owner)) {
            startSession(owner, listOfNotNull(stale?.let { authorizeKey(owner, game, it) }))
            return signer
        }
        if (stale != null) io.sendBasenet(owner, listOf(authorizeKey(owner, game, stale)))
        return signer
    }

    /**
     * Entering [game] with a session already live: grants this install's key for it if the store
     * lacks it, so play can start without another prompt. Only then — with the ledger at home
     * there is no session to play in yet, and the delegation that starts one carries the grant.
     */
    suspend fun consentIfDelegated(owner: String, game: VaultGame) {
        if (!isDelegated(owner)) return
        val stale = staleAuthorization(owner, game) ?: return
        io.sendBasenet(owner, listOf(authorizeKey(owner, game, stale)))
    }

    /** Whether this install's key still needs authorising for [game] — after a reinstall, a
     *  first visit, or once other devices' keys pushed it out of the store's ring. */
    suspend fun needsAuthorization(owner: String, game: VaultGame): Boolean =
        staleAuthorization(owner, game) != null

    /**
     * This install's session key, when the store does not hold it for [game]. The wallet
     * consenting as itself needs nothing. A temporary key counts only until it expires, by the
     * device's clock, which is close enough to decide a re-authorise by.
     */
    private suspend fun staleAuthorization(owner: String, game: VaultGame): String? {
        val signer = io.sessionKey(owner)
        if (signer == owner) return null
        val store = runCatching { io.readBasenet(VaultPda.session(owner)) }.getOrNull()?.data
            ?.let(SessionCodec::decode)
        val allowed = store?.allows(signer, game.program, io.nowSecs()) ?: false
        return signer.takeIf { !allowed }
    }

    /** Registers [key] for [game]: a permanent key for a kept login, the temporary slot for
     *  [KeepLoggedIn.TEMPORARY_KEY_SECS] otherwise. */
    private fun authorizeKey(owner: String, game: VaultGame, key: String) =
        VaultIx.authorizeSession(game, owner, key, KeepLoggedIn.sessionKeyExpiry(io.nowSecs()))

    private suspend fun authorizations(owner: String, games: List<VaultGame>) =
        games.mapNotNull { game -> staleAuthorization(owner, game)?.let { authorizeKey(owner, game, it) } }

    // ── the session ──────────────────────────────────────────────────────────

    /** Delegates the ledger, carrying [authorize] in the same signature. basenet. */
    private suspend fun startSession(owner: String, authorize: List<com.interstellargames.solana.Ix>) {
        awaitLedgerOwner(owner, ChainIds.DELEGATION, "The session didn't start in time — try again.") {
            io.sendBasenet(owner, authorize + VaultIx.delegateLedger(owner, owner))
        }
    }

    /**
     * Ends a live session so basenet owns the ledger again. Announces
     * [FundsPhase.ENDING_SESSION] only once there is a session to end.
     *
     * The vault is shared — a ledger can be delegated to any validator — so the router names the
     * one holding it and the (permissionless) undelegate goes there, signed by the session key,
     * which pays for the commit. The commit is then followed to basenet by its signature: the
     * ledger's basenet owner flips only once that lands, and the account push for it lags.
     */
    suspend fun bringHome(owner: String, onPhase: (FundsPhase) -> Unit = {}) {
        if (!isDelegated(owner)) return
        onPhase(FundsPhase.ENDING_SESSION)
        val ledger = VaultPda.ledger(owner)
        val endpoint = io.delegatedEndpoint(ledger)
            ?: throw IllegalStateException("Couldn't find the session holding your balance.")
        val sig = io.sendAt(endpoint, owner, listOf(VaultIx.undelegateLedger(io.sessionKey(owner), owner)))
        if (!awaitCommitHome(owner, ledger, endpoint, sig)) {
            awaitLedgerOwner(owner, VAULT_PROGRAM_ID, "The session didn't close in time — try again.") {}
        }
    }

    /**
     * Follows MagicBlock's ScheduledCommitSent logs from the undelegate to the base-chain commit
     * transaction, and confirms that. Returns false when the log chain can't be read, so the
     * caller falls back to watching the ledger's owner.
     */
    private suspend fun awaitCommitHome(owner: String, ledger: String, endpoint: String, undelegateSig: String): Boolean {
        val scheduled = io.rollupTxLogs(owner, endpoint, undelegateSig)?.let { commitSig(it, SCHEDULED) } ?: return false
        val basenetSig = io.rollupTxLogs(owner, endpoint, scheduled)?.let { commitSig(it, SCHEDULED_SENT) } ?: return false
        runCatching { io.awaitBasenetSignature(basenetSig) }
        return io.basenetOwner(ledger) == VAULT_PROGRAM_ID
    }

    private fun commitSig(logs: List<String>, marker: String): String? =
        logs.firstOrNull { it.contains(marker) }?.substringAfter(marker)?.trim()?.takeIf { it.isNotEmpty() }

    // ── funds ────────────────────────────────────────────────────────────────

    /**
     * Wallet into the vault, and straight back into a session: funding, the consent of every game
     * in [authorize] the store doesn't hold yet, and the delegation, in one signature. Opens the
     * ledger and its session store on first use; [lamports] may be 0. Returns the ledger as the
     * delegating transaction wrote it — the deposited balance, so nothing needs asking for after.
     */
    suspend fun deposit(
        owner: String,
        lamports: Long,
        authorize: List<VaultGame> = emptyList(),
        onPhase: (FundsPhase) -> Unit = {},
    ): LedgerState? {
        bringHome(owner, onPhase)
        val consent = authorizations(owner, authorize)
        return awaitLedgerOwner(owner, ChainIds.DELEGATION, "The session didn't start in time — try again.") {
            onPhase(FundsPhase.APPROVING)
            io.sendBasenet(
                owner,
                listOf(VaultIx.deposit(owner, ZERO_KEY, lamports)) + consent + VaultIx.delegateLedger(owner, owner),
            )
            // The funds and the delegation are the same transaction, so once it lands the only
            // thing left is for the base chain to show the ledger changing hands.
            onPhase(FundsPhase.STARTING_SESSION)
        }
    }

    /** Vault → wallet: all the SOL and every token. The off-ramp deliberately leaves the ledger on
     *  basenet — somebody taking their money out is done for now. */
    suspend fun withdrawAll(owner: String, onPhase: (FundsPhase) -> Unit = {}) =
        cashOut(owner, includeSol = true, onPhase = onPhase)

    /** Only the tokens — collect the winnings, keep playing on the SOL. */
    suspend fun withdrawTokens(owner: String, onPhase: (FundsPhase) -> Unit = {}) =
        cashOut(owner, includeSol = false, onPhase = onPhase)

    private suspend fun cashOut(owner: String, includeSol: Boolean, onPhase: (FundsPhase) -> Unit) {
        bringHome(owner, onPhase)
        // A read that fails must not read as "nothing to withdraw": the caller reports success
        // the moment this returns, and an unchanged balance under "everything is back in your
        // wallet" is the worst thing this flow can say.
        val unread = "Couldn't read your balance, so nothing was withdrawn — please try again."
        val read = runCatching { io.readBasenet(VaultPda.ledger(owner)) }.getOrElse { error(unread) }
            ?: error("Nothing to withdraw.")
        val ledger = LedgerCodec.decode(read.data) ?: error(unread)
        val tokens = ledger.balances.filterKeys { it != ZERO_KEY }.filterValues { it > 0L }
        val sol = if (includeSol) ledger.sol else 0L
        if (tokens.isEmpty() && sol == 0L) error("Nothing to withdraw.")
        if (tokens.isNotEmpty()) io.ensureMintFacts(tokens.keys.toList())
        onPhase(FundsPhase.WITHDRAWING)
        for ((mint, amount) in tokens) {
            io.sendBasenet(owner, listOf(
                SplIx.createAssociatedTokenIdempotent(owner, owner, mint),
                VaultIx.withdraw(owner, mint, amount),
            ))
        }
        if (sol > 0L) io.sendBasenet(owner, listOf(VaultIx.withdraw(owner, ZERO_KEY, sol)))
    }

    // ── waiting ──────────────────────────────────────────────────────────────

    /** Where an awaited account lives. */
    enum class Chain { BASENET, ROLLUP }

    /**
     * Fires [trigger] and waits for the ledger to change hands, returning it as the notification
     * carried it — the account exactly as the awaited transaction wrote it.
     */
    private suspend fun awaitLedgerOwner(
        owner: String,
        program: String,
        whenLate: String,
        trigger: suspend () -> Unit,
    ): LedgerState? {
        val update = awaitAccount(VaultPda.ledger(owner), Chain.BASENET, LEDGER_WAIT_MS, trigger) {
            it.owner == program
        } ?: error(whenLate)
        return update.data?.let(LedgerCodec::decode)
    }

    /**
     * Suspends until [address] satisfies [ready], and always unsubscribes on the way out.
     *
     * The subscription is opened before [trigger] runs, so there is no window to miss. The
     * account as it stands is read once after the trigger — it may already be what is being
     * waited for, and no notification would ever say so — and once more at the deadline: a
     * notification is the fast path, not the truth. On a chain this platform cannot subscribe to
     * at all, asking with a quick back-off is the only thing left.
     */
    suspend fun awaitAccount(
        address: String,
        chain: Chain,
        timeoutMs: Long,
        trigger: suspend () -> Unit,
        ready: (AccountUpdate) -> Boolean,
    ): AccountUpdate? {
        val hit = CompletableDeferred<AccountUpdate>()
        val watch = runCatching {
            val onUpdate: (AccountUpdate) -> Unit = { if (ready(it)) hit.complete(it) }
            when (chain) {
                Chain.BASENET -> io.watchBasenet(address, onUpdate)
                Chain.ROLLUP -> io.watchRollup(address, onUpdate)
            }
        }.getOrNull()

        try {
            trigger()
            readUpdate(address, chain)?.takeIf(ready)?.let { return it }
            if (watch == null) return askUntil(address, chain, timeoutMs, ready)
            withTimeoutOrNull(timeoutMs) { hit.await() }?.let { return it }
            return readUpdate(address, chain)?.takeIf(ready)
        } finally {
            withContext(NonCancellable) { runCatching { watch?.close() } }
        }
    }

    private suspend fun askUntil(
        address: String,
        chain: Chain,
        timeoutMs: Long,
        ready: (AccountUpdate) -> Boolean,
    ): AccountUpdate? {
        val deadline = TimeSource.Monotonic.markNow() + timeoutMs.milliseconds
        var wait = 250L
        while (deadline.hasNotPassedNow()) {
            delay(wait)
            wait = (wait * 2).coerceAtMost(3_000)
            readUpdate(address, chain)?.takeIf(ready)?.let { return it }
        }
        return null
    }

    /** The rollup answers bytes only; nothing asked of it turns on who owns the account. */
    private suspend fun readUpdate(address: String, chain: Chain): AccountUpdate? = runCatching {
        when (chain) {
            Chain.BASENET -> io.readBasenet(address)?.let { AccountUpdate(it.owner, it.data) }
            Chain.ROLLUP -> io.readRollup(address)?.let { AccountUpdate(null, it) }
        }
    }.getOrNull()
}

/**
 * One subscription that comes and goes as the thing it watches moves between chains.
 *
 * [want] is called from every notification, so it has to be safe to ask for the state it is
 * already in — it acts only on a change, and serialises those, so a burst of updates cannot
 * leave two subscriptions open or a closed one still running.
 */
class SwappableWatch(private val scope: CoroutineScope) {
    private val lock = Mutex()
    private var held: Subscription? = null
    private var wanted = false

    fun want(on: Boolean, start: suspend () -> Subscription?) {
        if (on == wanted) return
        wanted = on
        scope.launch {
            lock.withLock {
                if (wanted && held == null) {
                    held = runCatching { start() }.getOrNull()
                } else if (!wanted && held != null) {
                    runCatching { held?.close() }
                    held = null
                }
            }
        }
    }

    suspend fun close() {
        wanted = false
        lock.withLock {
            runCatching { held?.close() }
            held = null
        }
    }
}
