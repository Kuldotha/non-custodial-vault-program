package com.interstellargames.vault

import com.interstellargames.solana.Base58
import com.interstellargames.solana.ChainIds
import com.interstellargames.solana.Ix
import com.interstellargames.solana.IxAccount
import com.interstellargames.solana.MagicPda
import com.interstellargames.solana.Pda
import com.interstellargames.solana.ZERO_KEY
import com.interstellargames.solana.none
import com.interstellargames.solana.ro
import com.interstellargames.solana.rw
import com.interstellargames.solana.sg
import com.interstellargames.solana.sgw
import com.interstellargames.solana.u64le

/**
 * A game as the vault sees it: the program whose receipts it settles and whose session keys it
 * scopes, the PDA a settle names as the game's, and the treasuries whose ledgers follow the
 * player's in a settle, in the order the game's receipts index them.
 */
class VaultGame(val program: String, val house: String, val treasuries: List<String> = listOf(house))

/**
 * Instruction builders for the vault — the program that actually holds the money. Anchor
 * discriminators, so the leading eight bytes are the hash of the instruction's name.
 */
object VaultIx {
    private object Disc {
        val DEPOSIT = bytes(242, 35, 198, 137, 82, 225, 242, 182)
        val WITHDRAW = bytes(183, 18, 70, 156, 148, 109, 161, 34)
        val DELEGATE_LEDGER = bytes(159, 3, 197, 64, 7, 12, 101, 66)
        val UNDELEGATE = bytes(131, 148, 180, 198, 91, 104, 42, 238)
        val SETTLE_RECEIPT = bytes(216, 17, 200, 111, 7, 3, 233, 182)
        val AUTHORIZE_SESSION = bytes(187, 218, 251, 161, 99, 40, 34, 34)
        val REVOKE_SESSION = bytes(86, 92, 198, 120, 144, 2, 7, 194)

        private fun bytes(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }
    }

    private fun vaultIx(data: ByteArray, keys: List<IxAccount>) = Ix(VAULT_PROGRAM_ID, keys, data)

    /**
     * The token slots of a SOL movement are placeholders the program never reads. The System
     * Program is a native program and cannot be a writable account, so it rides read-only.
     */
    private fun tokenSlot(isSol: Boolean, owner: String, mint: String, tokenProgram: String) =
        if (isSol) ro(ZERO_KEY) else rw(Pda.associatedToken(owner, mint, tokenProgram))

    /**
     * Deposit — wallet into the vault. **basenet only.**
     *
     * This is also the only way a ledger comes into existence: it opens the ledger, its
     * permission account and its session store on first use and keeps the free-slot headroom,
     * so there is nothing to create beforehand. `amount` may be zero, which is how you open a
     * ledger without funding it. For SOL, pass [ZERO_KEY] as the mint.
     */
    fun deposit(owner: String, mint: String, amount: Long, tokenProgram: String = ChainIds.TOKEN): Ix {
        val isSol = mint == ZERO_KEY
        val ledger = VaultPda.ledger(owner)
        val data = Disc.DEPOSIT + Base58.decode(mint) + u64le(amount) +
            none() + none()   // min_free, slot_increase — defaults

        val keys = mutableListOf(
            sgw(owner),
            rw(ledger),
            rw(MagicPda.permission(ledger)),
            ro(ChainIds.PERMISSION),
            rw(VaultPda.reserve),
            tokenSlot(isSol, VaultPda.reserve, mint, tokenProgram),
            tokenSlot(isSol, owner, mint, tokenProgram),
            ro(tokenProgram),
            ro(ZERO_KEY),
            rw(VaultPda.session(owner)),
        )
        // Token-2022 transfers are checked, and checked transfers carry the mint.
        if (!isSol && tokenProgram == ChainIds.TOKEN_2022) keys += ro(mint)
        return vaultIx(data, keys)
    }

    /**
     * Withdraw — vault back to the wallet. **basenet only.**
     *
     * The destination is derived from the signer and never passed, which is what makes this
     * same-owner-only: there is no way to spell a withdrawal to somebody else.
     */
    fun withdraw(owner: String, mint: String, amount: Long, tokenProgram: String = ChainIds.TOKEN): Ix {
        val isSol = mint == ZERO_KEY
        val data = Disc.WITHDRAW + Base58.decode(mint) + u64le(amount)

        val keys = mutableListOf(
            sgw(owner),
            rw(VaultPda.ledger(owner)),
            rw(VaultPda.reserve),
            tokenSlot(isSol, VaultPda.reserve, mint, tokenProgram),
            tokenSlot(isSol, owner, mint, tokenProgram),
            ro(tokenProgram),
            ro(ZERO_KEY),
        )
        if (!isSol && tokenProgram == ChainIds.TOKEN_2022) keys += ro(mint)
        return vaultIx(data, keys)
    }

    /**
     * Hands the player's ledger to the TEE validator so the game can settle against it.
     * **basenet only**. No permission account is involved — the vault gates who may pay, not
     * who may read, and the deposit that created the ledger already created its permission.
     *
     * Account order is the `#[delegate]` macro's: it inserts buffer/record/metadata *before*
     * the delegated field, then appends the three programs.
     */
    fun delegateLedger(payer: String, owner: String): Ix {
        val ledger = VaultPda.ledger(owner)
        return vaultIx(
            Disc.DELEGATE_LEDGER + byteArrayOf(1) + Base58.decode(ChainIds.ER_VALIDATOR), // Some(validator)
            listOf(
                sgw(payer),
                sg(owner),
                rw(VaultPda.delegationBuffer(ledger)),
                rw(MagicPda.delegationRecord(ledger)),
                rw(MagicPda.delegationMetadata(ledger)),
                rw(ledger),
                ro(VAULT_PROGRAM_ID),
                ro(ChainIds.DELEGATION),
                ro(ZERO_KEY),
            ),
        )
    }

    /**
     * Ends the session and returns the ledger to basenet. **Sent to the rollup, not basenet.**
     * Permissionless: `payer` funds the commit and must be the fee payer, while `owner` only
     * names whose ledger to bring home. The commit is implicit in undelegating.
     */
    fun undelegateLedger(payer: String, owner: String): Ix = vaultIx(
        Disc.UNDELEGATE,
        listOf(
            sgw(payer),
            sg(payer),
            rw(VaultPda.ledger(owner)),
            ro(ChainIds.MAGIC_PROGRAM),
            rw(ChainIds.MAGIC_CONTEXT),
            rw(ChainIds.EPHEMERAL_VAULT),
        ),
    )

    /**
     * Settles a receipt — the only instruction that touches a ledger.
     *
     * **Rollup only.** The receipt's consenter — the session key that wrote it — must sign here
     * too: the vault requires that signature and checks it against the ledger's owner or the
     * keys in the player's session store, which follows the ledgers. The movements were fixed
     * at creation, so settle only executes them and fires the callback; [forwarded] is the
     * game's own callback accounts.
     */
    fun settleReceipt(game: VaultGame, user: String, consenter: String, forwarded: List<IxAccount>): Ix = vaultIx(
        Disc.SETTLE_RECEIPT,
        listOf(
            rw(VaultPda.receipt(game.program, consenter)),
            ro(game.house),
            sg(consenter),
            ro(game.program),
            ro(VaultPda.authority),
            rw(ChainIds.EPHEMERAL_VAULT),
            ro(ChainIds.MAGIC_PROGRAM),
            rw(ChainIds.MAGIC_CONTEXT),
            rw(VaultPda.ledger(user)),
        ) + game.treasuries.map { rw(VaultPda.ledger(it)) } + listOf(
            ro(VaultPda.session(user)),
        ) + forwarded,
    )

    /**
     * Lets the game's session key consent to the player's ledger debits: into the game's ring
     * of five persisted keys, or with [expiresAt] (unix seconds) into its one temporary slot.
     * **basenet only.** The owner signs, and pays the entry's rent the first time; the store is
     * created here if the ledger predates it. Scoped to the game's program: a key minted for one
     * game consents for that game and nothing else.
     */
    fun authorizeSession(game: VaultGame, owner: String, key: String, expiresAt: Long = 0L): Ix = vaultIx(
        Disc.AUTHORIZE_SESSION + Base58.decode(game.program) + Base58.decode(key) + u64le(expiresAt),
        listOf(
            rw(VaultPda.session(owner)),
            sgw(owner),
            ro(ZERO_KEY),
        ),
    )

    /**
     * Forgets one of the game's session keys; the zero key drops the game's entry whole and
     * refunds its rent. **basenet only.** The owner signs.
     */
    fun revokeSession(game: VaultGame, owner: String, key: String = ZERO_KEY): Ix = vaultIx(
        Disc.REVOKE_SESSION + Base58.decode(game.program) + Base58.decode(key),
        listOf(
            rw(VaultPda.session(owner)),
            sgw(owner),
        ),
    )
}
