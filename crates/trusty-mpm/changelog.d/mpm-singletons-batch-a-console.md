Added

- **The `tm` welcome banner's services block leads with a `console` row carrying the console's own port.** The block named only the daemon, so the address an operator opens in a browser was never printed. The port comes from the trusty-console discovery record (its documented default when none exists) and is shown only when a bounded TCP probe answers; the daemon row stays port-less, as [#6869](https://github.com/bobmatnyc/trusty-tools/issues/6869) decided ([#6761](https://github.com/bobmatnyc/trusty-tools/issues/6761))
