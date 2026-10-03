Changed

- The `tm issue` state-model validator rejects a model in which two states share one `label.name`, compared case-insensitively, and names both states (#8703). A model that loads today can now fail to load; give each labelled state its own label.
