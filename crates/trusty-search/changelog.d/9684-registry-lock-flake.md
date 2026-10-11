Fixed
- Tests that run a first search no longer race the `#[serial]` tests that repoint `TRUSTY_DATA_DIR`, so `create_refuses_a_data_dir_inside_the_work_tree` no longer finds a stray `indexes.toml.lock` in its work tree (#9684).
