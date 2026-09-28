package com.interstellargames.vault

import com.interstellargames.solana.LeReader
import com.interstellargames.solana.ZERO_KEY

class LedgerState(
    val owner: String,
    val pdaAuth: Boolean,
    val capacity: Int,
    /** mint (base58) → raw amount; slot 0's SOL rides under [ZERO_KEY] */
    val balances: Map<String, Long>,
    val authorized: String?,
) {
    val sol: Long get() = balances[ZERO_KEY] ?: 0L
}

/** A vault ledger: a 116-byte header, then 40 bytes per slot (mint, amount). */
object LedgerCodec {
    private const val HEADER = 116
    private const val ENTRY = 40

    fun decode(data: ByteArray): LedgerState? {
        if (data.size < HEADER) return null
        val b = LeReader(data)
        b.i64()                                  // anchor discriminator
        val owner = b.key()
        val pdaAuth = b.u8() != 0
        b.skip(1)                                // bump
        b.skip(6)                                // padding
        b.skip(32)                               // rent_payer — who gets the rent when it closes
        val authorized = b.key()
        val capacity = b.i32()

        val balances = mutableMapOf<String, Long>()
        for (i in 0 until capacity) {
            val off = HEADER + i * ENTRY
            if (off + ENTRY > data.size) break
            b.at = off
            val mint = b.key()
            val amount = b.i64()
            val isFreeSlot = i != 0 && mint == ZERO_KEY
            if (isFreeSlot || amount == 0L) continue
            balances[mint] = amount
        }
        // A zero key means revoked, or never set.
        return LedgerState(owner, pdaAuth, capacity, balances, authorized.takeIf { it != ZERO_KEY })
    }
}
