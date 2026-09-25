Fixed

- The memory secret filter (`check_secret`) no longer refuses relative source paths whose file or directory names are long CamelCase identifiers, such as `src/main/java/com/example/ReservationForecastAdjustmentServiceImpl.java`. These paths were most of the memories a kuzu-memory import refused ([#277](https://github.com/bobmatnyc/trusty-tools/issues/277), [#8589](https://github.com/bobmatnyc/trusty-tools/issues/8589))
- Google Docs, Sheets, Slides and Drive URLs (`https://docs.google.com/spreadsheets/d/<id>`) now store. The document id is admitted only in its URL position on a Google document host. A bare id is still refused ([#8589](https://github.com/bobmatnyc/trusty-tools/issues/8589))
