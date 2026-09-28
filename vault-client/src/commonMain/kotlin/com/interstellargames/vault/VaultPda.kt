package com.interstellargames.vault

import com.interstellargames.solana.Base58
import com.interstellargames.solana.MagicPda
import com.interstellargames.solana.Pda

/** The vault program, the same id on devnet and mainnet. */
const val VAULT_PROGRAM_ID = "VAULTrDSUBZ8AXL2kGVYE8eKAn7tgWXRPAevNGUsyTV"

/** The vault's addresses, the same for every game, as base58 strings. */
object VaultPda {
    private fun seed(s: String) = s.encodeToByteArray()
    private fun raw(key58: String) = Base58.decode(key58)
    private fun vault(vararg seeds: ByteArray) = Pda.find58(seeds.toList(), VAULT_PROGRAM_ID)

    fun ledger(owner58: String): String = vault(seed("ledger"), raw(owner58))

    /** The wallet's session store: the keys its game clients may consent with, one entry per game. */
    fun session(owner58: String): String = vault(seed("session"), raw(owner58))

    /** A consenter's receipt for one game's program. */
    fun receipt(program58: String, consenter58: String): String =
        vault(seed("receipt"), raw(program58), raw(consenter58))

    val authority: String by lazy { vault() }
    val reserve: String by lazy { vault(seed("vault")) }

    fun delegationBuffer(ledger58: String): String = MagicPda.delegationBuffer(ledger58, VAULT_PROGRAM_ID)
}
