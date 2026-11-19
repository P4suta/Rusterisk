# Contributing

Open an issue describing the behavior, why it matters, and how to observe it.
Use an isolated configuration and omit credentials and personal call data from examples.
Report suspected vulnerabilities through the [security policy](SECURITY.md).

Run the same fast checks used by GitHub Actions.

```sh
mise install
mise x -- just check
mise x -- just security
```

Use the [compatibility contract](docs/compatibility.md) to choose assertions for Asterisk behavior.
Exercise audio changes with the [reference comparisons](docs/reference-comparisons.md).
