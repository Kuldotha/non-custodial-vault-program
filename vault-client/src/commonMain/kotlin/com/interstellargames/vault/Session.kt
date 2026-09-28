package com.interstellargames.vault

import com.interstellargames.solana.LeReader
import com.interstellargames.solana.ZERO_KEY

/** One game's keys in a wallet's session store: the temporary slot and the ring of five. */
class SessionEntry(
    val program: String,
    val temporary: String?,
    val expiresAt: Long,
    val ring: List<String>,
) {
    fun allows(key: String, nowSecs: Long): Boolean =
        (temporary == key && nowSecs < expiresAt) || key in ring
}

/** The wallet's session store, `["session", owner]` at the vault: one entry per game. */
class SessionState(val owner: String, val entries: List<SessionEntry>) {
    fun allows(key: String, program: String, nowSecs: Long): Boolean =
        entries.firstOrNull { it.program == program }?.allows(key, nowSecs) ?: false
}

/** A 48-byte header, then 232 bytes per program: program, temporary key, its expiry, five ring
 *  keys. A zero key is an empty slot. */
object SessionCodec {
    private const val HEADER = 48
    private const val ENTRY = 232
    private const val RING = 5

    fun decode(data: ByteArray): SessionState? {
        if (data.size < HEADER) return null
        val b = LeReader(data)
        b.i64()                                  // anchor discriminator
        val owner = b.key()
        b.skip(4)                                // bump, padding
        val count = b.i32()
        val entries = mutableListOf<SessionEntry>()
        for (i in 0 until count) {
            val off = HEADER + i * ENTRY
            if (off + ENTRY > data.size) break
            b.at = off
            val program = b.key()
            val temporary = b.key().takeIf { it != ZERO_KEY }
            val expiresAt = b.i64()
            val ring = (0 until RING).map { b.key() }.filter { it != ZERO_KEY }
            entries += SessionEntry(program, temporary, expiresAt, ring)
        }
        return SessionState(owner, entries)
    }
}
