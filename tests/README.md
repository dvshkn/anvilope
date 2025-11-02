Anvilope Tests
==============

All commands listed here should be run from the root of the Anvilope repository.

## Running Unit Tests

```
$ cargo utest
```


## Running Integration Tests

The config file for integration tests is located at `tests/data/anvilope.toml`.

Integration tests require that both the LLM inference server and a
[Greenmail](https://github.com/greenmail-mail-test/greenmail) test email server
be running. The easiest way to run Greenmail is using podman.

```
# Tab 1: start LLM inference server
$ ollama serve

# Tab 2: start Greenmail w/ podman
$ scripts/run_greenmail.sh

# Tab 3: run integration tests
$ cargo itest
```
