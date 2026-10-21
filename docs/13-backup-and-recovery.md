# 13 · Encrypted backups and recovery

Backups are offline snapshots of one LayerAgent home. Stop every profile using that home first. A shared runtime/exclusive backup lease and older profile-lock checks prevent a supported runtime from racing the snapshot. Do not let unrelated processes modify the home during backup.

## Create and verify

Keep the private identity outside the runtime home and outside your source tree:

```bash
layeragent backup keygen --identity /secure/location/layeragent-backup.identity
```

The command prints a public `age1...` recipient. Store the private identity separately; losing it makes existing archives unrecoverable.

```bash
layeragent stop
layeragent backup create --output /backups/layeragent-backup-00000000000000000001.age --recipient YOUR_AGE_RECIPIENT
layeragent backup verify --archive /backups/layeragent-backup-00000000000000000001.age --identity /secure/location/layeragent-backup.identity
layeragent backup restore --archive /backups/layeragent-backup-00000000000000000001.age --identity /secure/location/layeragent-backup.identity --destination /new/private/layeragent-home
```

Restore requires a new, nonexistent destination. It verifies authenticated encryption and manifest hashes before publishing the restored home. Wrong keys, modified archives, invalid paths, symlinks and configured size/file-count violations fail explicitly.

## Snapshot boundary

| Included | Excluded |
| --- | --- |
| Main/profile configuration references | Runtime sockets, locks and endpoint tokens |
| Tenant databases through SQLite backup API | `service.env`, `.env` and key/identity files |
| Tenant artifacts and principal skills/state | Embedding/browser caches and downloaded runtimes |
| Archived/draft skill state within the allowlist | OS keyring credentials and model-provider login sessions |

Backups contain potentially sensitive project history inside the encrypted payload. They are not a credential vault, a source release archive or an automatic redaction of historical data. Reconnect credential references/keyring logins after restore and review project paths.

## Retention

```bash
layeragent backup retain --directory /backups --keep 7
```

Retention only considers archives matching the documented `layeragent-backup-<20 digit timestamp>.age` naming convention. It does not schedule backups. Use your service scheduler to stop the runtime, create an archive, verify it, restart and apply retention. Treat a failed verification as an operational failure.

Run `python3 tests/backup_smoke.py` after building the CLI for a complete local create/verify/restore and tamper/wrong-key test.

## Optional maintenance timer

`deploy/layeragent-backup.service` and `deploy/layeragent-backup.timer` are user-service templates. Review their paths before installing. `scripts/backup_home.py` reads only a public recipient file, stops an explicitly named running service, creates the encrypted archive, applies retention and restores the service in a finally block. Stop other profiles separately. Because the script has no private identity, it cannot perform a decrypt/restore drill; schedule that verification independently.
