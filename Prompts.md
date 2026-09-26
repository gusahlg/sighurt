# Pull Request

Run cargo fmt --all
Run cargo clippy --workspace --all-targets -- -D warnings
Run cargo test --workspace
Run cargo check --target wasm32-unknown-unknown --workspace --exclude sighurt-browser --exclude sighurt-core --exclude sighurt-ui --exclude sighurt-engine-wasm --exclude fullstack-notes-backend

The Servo engine (sighurt-engine-servo) is its own workspace and is not covered by the commands above. If you changed it or sighurt-ipc, check it separately:

Run cargo fmt --manifest-path sighurt-engine-servo/Cargo.toml
Run cargo clippy --manifest-path sighurt-engine-servo/Cargo.toml -- -D warnings

If the tests fail, then you need to fix the issues and run the tests again.

If the tests pass, then you can commit the changes.

Git add all the files. Create detailed commit message. Create new branch.
