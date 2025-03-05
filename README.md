# Oni

Prototype of a rsync-like tool.

Remote file syncing (delta-transferred), in archiving and recursive mode by default:

```bash
# start the listening server on the remote side
cargo run -- --server=<ip-addr>

# transfer the data from the client
cargo run -- local-folder <ip-addr>:~/folder
```

The copying also works for local-to-local syncing.

```bash
# verbose logs included
cargo run -- --verbose local-folder another-local-folder
```
