Documentation
- Document that broker's scope gate is the authority boundary for `invoke_menu`, preventing out-of-scope calls from reaching the driver
- Add broker unit test proving out-of-scope `invoke_menu` calls are refused with `pid_out_of_scope` or `window_out_of_scope` before driver activation
