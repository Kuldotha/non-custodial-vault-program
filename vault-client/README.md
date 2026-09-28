# vault-client

The vault's client for Kotlin Multiplatform (Android and the browser): the program id, `VaultPda`
(ledger, session store, receipt, reserve, authority), `VaultIx` (deposit, withdraw, delegate,
undelegate, settle, session keys) and the ledger and session-store decoders. Built on
`solana-client`, which the consuming build includes too:

```kotlin
include(":solana-client")
project(":solana-client").projectDir = file("../solana-client")
include(":vault-client")
project(":vault-client").projectDir = file("../vault-program/vault-client")
```
