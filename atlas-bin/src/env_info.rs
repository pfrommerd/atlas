// Provides methods for interacting with the environment
// that are portable across webassembly + native platforms

pub fn user() -> String {
    return whoami::username();
}

pub fn host() -> String {
    return hostname::get().unwrap().into_string().unwrap();
}

// pub fn current_dir_pretty() -> String {
//     let mut dir = std::env::current_dir().unwrap().into_os_string().into_string().unwrap();
//     let home = dirs::home_dir().unwrap().into_os_string().into_string().unwrap();
//     if dir.starts_with(&home) {
//         let after_home = dir.split_off(home.len());
//         dir = "~".to_owned();
//         dir.push_str(&after_home);
//     }
//     dir
// }