//! Tests for `pod_env_dump` (#7648). Split out so the rule file stays under
//! the production SLOC cap; this `_tests.rs` file is a test file.

use super::*;

fn assert_denies(commands: &[&str]) {
    for command in commands {
        assert!(
            evaluate_pod_env_dump_command(command).is_some(),
            "`{command}` must deny"
        );
    }
}

fn assert_allows(commands: &[&str]) {
    for command in commands {
        assert_eq!(
            evaluate_pod_env_dump_command(command),
            None,
            "`{command}` must allow"
        );
    }
}

#[test]
fn refuses_an_unscoped_pod_env_dump() {
    assert_denies(&[
        "kubectl exec my-pod -- env",
        "kubectl exec my-pod -- env | grep -i LOG",
        "kubectl -n prod exec -it my-pod -c app -- /usr/bin/env",
        "kubectl exec deploy/api -- printenv",
        "kubectl exec my-pod -- printenv -0",
        "kubectl exec my-pod -- env -i -u X",
        "kubectl exec my-pod -- env -- ",
        "kubectl exec my-pod -- sh -c 'env | sort'",
        "kubectl exec my-pod -- bash -lc \"printenv\"",
        "kubectl exec my-pod -- env FOO=1 sh -c env",
        "kubectl exec my-pod -- export",
        "kubectl exec my-pod -- sh -c set",
        "kubectl exec my-pod -- cat /proc/1/environ",
        "oc exec my-pod -- env",
        "kubectl exec my-pod env",
        "cd /tmp && kubectl exec my-pod -- env",
    ]);
}

/// 🔴 REGRESSION (#7648 critic HIGH 1): `oc rsh` runs a remote command
/// with no `exec` and no `--`. Both deny rows ALLOWED on `8dfcf2e1e`.
#[test]
fn refuses_an_oc_rsh_env_dump() {
    assert_denies(&[
        "oc rsh mypod env",
        "oc rsh mypod -- env",
        "oc -n prod rsh -c app mypod printenv",
        "oc rsh --shell=/bin/bash mypod sh -c 'env | sort'",
    ]);
    assert_allows(&[
        "oc rsh mypod ls",
        "oc rsh mypod",
        "oc rsh mypod printenv HOME",
    ]);
}

/// 🔴 REGRESSION (#7648 round-3 critic HIGH): an option the parser did not
/// list as value-taking shifted the parse, so `oc rsh --as adminuser mypod
/// env` read `adminuser` as the pod and `mypod env` as the command. ALLOWED
/// on `bec394551`. An unrecognised option now counts both ways.
#[test]
fn refuses_an_oc_rsh_dump_behind_an_unlisted_value_flag() {
    assert_denies(&[
        "oc rsh --as adminuser mypod env",
        "oc rsh --as-group admins mypod printenv",
        "oc rsh --request-timeout 30s mypod cat /proc/1/environ",
        "oc rsh -s https://api.example:6443 mypod env",
        "oc rsh --as adminuser --token t mypod sh -c env",
        "oc --as rsh rsh mypod env",
        "kubectl exec --as adminuser mypod env",
        "oc rsh --as=adminuser mypod env",
    ]);
    assert_allows(&[
        "oc rsh --as adminuser mypod ls",
        "oc rsh --as=adminuser mypod ls",
        "oc rsh --as adminuser mypod",
        "oc rsh -t mypod printenv HOME",
    ]);
}

/// 🔴 REGRESSION (#7648 round-3 owner ruling, fix the class): `docker exec`
/// and its kin dump a container's injected secrets the same way a pod exec
/// does. Every deny row ALLOWED on `bec394551`.
#[test]
fn refuses_a_container_env_dump() {
    assert_denies(&[
        "docker exec c env",
        "docker exec -it c printenv",
        "docker container exec c env",
        "podman exec c cat /proc/1/environ",
        "podman container exec -u root c env",
        "nerdctl exec c env",
        "docker exec -e FOO=1 -u root -w /app c env",
        "docker exec --env FOO=1 c sh -c 'env | sort'",
        "docker exec --detach-keys ctrl-x c export",
        "docker --context prod exec c env",
        "docker compose exec api env",
        "docker-compose exec api printenv",
        "sudo docker exec c env | grep KEY",
        "docker exec -- c env",
        // #8523 round 4: `-T, --no-tty` (lowercase) is `docker compose exec`'s
        // real flag, not `--no-TTY`.
        "docker compose exec --no-tty api env",
        "docker compose exec -T api env",
    ]);
    assert_allows(&[
        "docker exec c ls",
        "docker exec -it c sh",
        "docker exec c printenv HOME",
        "docker exec -e FOO=1 c node app.js",
        "docker ps",
        "docker logs c | grep env",
        "podman exec c ls /app",
        "docker compose exec --no-tty api sh",
    ]);
}

#[test]
fn allows_a_scoped_pod_command() {
    assert_allows(&[
        "kubectl exec my-pod -- printenv LOG_LEVEL",
        "kubectl exec my-pod -- sh -c 'echo $LOG_LEVEL'",
        "kubectl exec my-pod -- env FOO=1 node app.js",
        "kubectl exec my-pod -- ls /app",
        "kubectl exec my-pod printenv LOG_LEVEL",
        "kubectl exec -it my-pod -- sh",
        "kubectl get pods -n prod",
        "kubectl logs my-pod | grep env",
        "env | grep PATH",
        "printenv HOME",
    ]);
}

/// Error arms: an unlexable segment naming the shape, an unreadable inner
/// script, and nesting past the bound all fail closed.
#[test]
fn refuses_what_it_cannot_read() {
    assert_denies(&[
        "kubectl exec my-pod -- env 'unclosed",
        "kubectl exec my-pod -- sh -c 'env \"unclosed'",
        "kubectl exec p -- env env env env env env true",
        "docker exec c env 'unclosed",
    ]);
    assert!(dumps_environment(&["true".to_string()], MAX_DEPTH + 1));
}
