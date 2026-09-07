use std::collections::HashMap;
use std::sync::Mutex;

use once_cell::sync::Lazy;

static INJECTED_ENV: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn set_env_vars(vars: &HashMap<String, String>) -> usize {
    let mut env = INJECTED_ENV.lock().unwrap();
    for (k, v) in vars {
        env.insert(k.clone(), v.clone());
    }
    env.len()
}

#[allow(dead_code)]
pub fn get_injected_env() -> HashMap<String, String> {
    INJECTED_ENV.lock().unwrap().clone()
}

#[allow(dead_code)]
pub fn apply_to_command(cmd: &mut std::process::Command) {
    let env = INJECTED_ENV.lock().unwrap();
    for (k, v) in env.iter() {
        cmd.env(k, v);
    }
}

pub fn apply_to_tokio_command(cmd: &mut tokio::process::Command) {
    let env = INJECTED_ENV.lock().unwrap();
    for (k, v) in env.iter() {
        cmd.env(k, v);
    }
}

#[allow(dead_code)]
pub fn clear_env() {
    INJECTED_ENV.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn set_and_get_env() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_env();
        let mut vars = HashMap::new();
        vars.insert("FOO".to_string(), "bar".to_string());
        vars.insert("BAZ".to_string(), "42".to_string());

        let count = set_env_vars(&vars);
        assert_eq!(count, 2);

        let stored = get_injected_env();
        assert_eq!(stored.get("FOO").unwrap(), "bar");
        assert_eq!(stored.get("BAZ").unwrap(), "42");
    }

    #[test]
    fn merge_overwrites_existing() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_env();
        let mut vars = HashMap::new();
        vars.insert("KEY".to_string(), "old".to_string());
        set_env_vars(&vars);

        let mut vars2 = HashMap::new();
        vars2.insert("KEY".to_string(), "new".to_string());
        set_env_vars(&vars2);

        let stored = get_injected_env();
        assert_eq!(stored.get("KEY").unwrap(), "new");
    }

    #[test]
    fn clear_removes_all() {
        let _guard = TEST_LOCK.lock().unwrap();
        clear_env();
        let mut vars = HashMap::new();
        vars.insert("X".to_string(), "1".to_string());
        set_env_vars(&vars);

        clear_env();
        assert!(get_injected_env().is_empty());
    }
}
