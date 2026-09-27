Fixed
- The `ORT_DYLIB_PATH` setup note printed after installing a load-dynamic trusty-search asset now points at ONNX Runtime 1.24.2 instead of 1.20.1. The bundled `ort` 2.0.0-rc.12 refuses any runtime below 1.24, so following the old note left the embedding daemon unable to start ([#8612](https://github.com/bobmatnyc/trusty-tools/issues/8612))
