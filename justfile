
hello: 
  echo "Just hello!"

test *ARGS:
  export RUST_LOG="iroh=off,ddcoin=debug"
  cargo test --features test_util {{ARGS}}

build:
  cargo build

propagator: build
  #!/usr/bin/env bash
  export RUST_LOG="iroh=off,ddcoin=info"
  ./target/debug/propagator

client: build
  #!/usr/bin/env bash
  export RUST_LOG="iroh=off"
  ./target/debug/client
