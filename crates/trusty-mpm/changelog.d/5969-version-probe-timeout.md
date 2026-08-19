Fixed

- The `claude --version` probe behind output-style detection now gives up after 5 seconds, killing and reaping the probe child, instead of waiting forever — a wedged `claude` binary used to hang session launch and every prompt-composition path and leave orphaned probe processes behind (#5969).
