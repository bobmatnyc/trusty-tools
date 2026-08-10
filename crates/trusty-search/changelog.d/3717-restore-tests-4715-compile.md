Fixed

- `cargo test -p trusty-search` compiles again. #5345 gave `index_status` and
  `chunks` a JSON body, changing their error type from `StatusCode` to
  `(StatusCode, Json<Value>)`, but two `assert_eq!` calls in
  `deleted_cold_parked_index_is_404_not_a_permanent_503` still compared the
  whole tuple against a bare `StatusCode`, so the lib-test target failed to
  build and took every gate on `main` with it. The three guards that test pins
  now destructure the response and assert both the 404 and the
  `unknown index: <id>` body, so a regression that returns 404 while still
  advertising `restore_via` is caught rather than passing on the code alone.
