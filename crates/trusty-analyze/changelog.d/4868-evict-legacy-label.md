Fixed

- `service install` evicts `com.trusty.trusty-analyze`, the label an older
  installer registered. The registry recorded it as a legacy alias and nothing
  acted on it, so the record meant nothing on a host that needed it (#4868)
