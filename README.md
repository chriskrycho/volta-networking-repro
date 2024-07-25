# Volta networking issue repro

For trying to sort out what in the world is going on in [volta-cli/volta#1744](https://github.com/volta-cli/volta/issues/1744).

Folks testing should be able to:

- clone this repo
- `cargo build --release`
- `vnr https://nodejs.org/dist/v20.0.0/node-v20.0.0-darwin-arm64.tar.gz ~/Desktop` (or similar – substitute whatever package and location you want)

This will *very* noisily trace all output. If it reproduces the problems we are seeing in that issue, we have a useful minimal reproduction. If it does *not*, that will also tell us something interesting.
