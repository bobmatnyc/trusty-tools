Fixed

- A `tm sessions decommission` refused by the shared-workspace guard now answers HTTP 409 Conflict and the CLI prints the guard's reason — naming the sibling session that still claims the directory — instead of an HTTP 500 whose body was discarded (refs [#7877](https://github.com/bobmatnyc/trusty-tools/issues/7877))
