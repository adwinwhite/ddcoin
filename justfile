
hello: 
  echo "Just hello!"

test:
  cargo test --features test_util

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
