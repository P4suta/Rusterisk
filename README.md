# Rusterisk

Run phone and browser calls with an Asterisk-compatible control API.

## Installation

```sh
git clone https://github.com/P4suta/Rusterisk.git
cd Rusterisk
mise install
mise x -- cargo install --locked --path crates/cli
```

## Usage

```sh
mise x -- cargo run --locked --package rusterisk-server --bin rusterisk -- --config-dir config --data-dir .cache/rusterisk-data
```

## Documentation

[Read the documentation](docs/README.md).

## License

Licensed under the [Apache License, Version 2.0](LICENSE-APACHE) or the [Massachusetts Institute of Technology (MIT) license](LICENSE-MIT), at your option.

Contributions use both licenses unless you state other terms.
