/// Checks if the application context menu option is registered in the Windows Explorer registry.
#[cfg(target_os = "windows")]
pub fn check_registry_integration() -> bool {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    let folder_ok = hkcu
        .open_subkey_with_flags(
            r"Software\Classes\Directory\shell\RemoveEmptyDirs\command",
            KEY_READ,
        )
        .is_ok();

    let bg_ok = hkcu
        .open_subkey_with_flags(
            r"Software\Classes\Directory\Background\shell\RemoveEmptyDirs\command",
            KEY_READ,
        )
        .is_ok();

    // Return true if either the directory or background context menu is registered
    folder_ok || bg_ok
}

/// Registers or unregisters the "Remove empty folders here" option in the Windows Explorer context menu.
/// This edits the HKCU branch, meaning it does NOT require UAC Administrator escalation.
#[cfg(target_os = "windows")]
pub fn set_registry_integration(integrate: bool) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    // Path for right-clicking directly on a folder icon
    let folder_path = r"Software\Classes\Directory\shell\RemoveEmptyDirs";
    // Path for right-clicking inside an empty space (background) of an opened folder
    let bg_path = r"Software\Classes\Directory\Background\shell\RemoveEmptyDirs";

    if integrate {
        let current_exe = std::env::current_exe()
            .map_err(|e| format!("Failed to resolve current executable path: {}", e))?;

        let command_str_folder = format!("\"{}\" \"%1\"", current_exe.to_string_lossy());
        // Background registry path passes '%V' which refers to the current working directory
        let command_str_bg = format!("\"{}\" \"%V\"", current_exe.to_string_lossy());

        // 1. Register Folder Icon Context Menu
        let (key, _) = hkcu
            .create_subkey_with_flags(folder_path, KEY_WRITE)
            .map_err(|e| e.to_string())?;
        key.set_value("", &"Remove empty folders here")
            .map_err(|e| e.to_string())?;
        key.set_value("Icon", &current_exe.to_string_lossy().as_ref())
            .map_err(|e| e.to_string())?;

        let (cmd_key, _) = key
            .create_subkey_with_flags("command", KEY_WRITE)
            .map_err(|e| e.to_string())?;
        cmd_key
            .set_value("", &command_str_folder)
            .map_err(|e| e.to_string())?;

        // 2. Register Folder Background Context Menu
        let (bg_key, _) = hkcu
            .create_subkey_with_flags(bg_path, KEY_WRITE)
            .map_err(|e| e.to_string())?;
        bg_key
            .set_value("", &"Remove empty folders here")
            .map_err(|e| e.to_string())?;
        bg_key
            .set_value("Icon", &current_exe.to_string_lossy().as_ref())
            .map_err(|e| e.to_string())?;

        let (cmd_bg_key, _) = bg_key
            .create_subkey_with_flags("command", KEY_WRITE)
            .map_err(|e| e.to_string())?;
        cmd_bg_key
            .set_value("", &command_str_bg)
            .map_err(|e| e.to_string())?;
    } else {
        let _ = hkcu.delete_subkey_all(folder_path);
        let _ = hkcu.delete_subkey_all(bg_path);
    }
    Ok(())
}

/// Fallback for non-Windows operating systems. Always returns false.
#[cfg(not(target_os = "windows"))]
pub fn check_registry_integration() -> bool {
    false
}

/// Fallback for non-Windows operating systems. Does nothing and returns Ok.
#[cfg(not(target_os = "windows"))]
pub fn set_registry_integration(_integrate: bool) -> Result<(), String> {
    Ok(())
}

/// Lightweight, zero-dependency Windows UAC verification using native OS tokens.
#[cfg(target_os = "windows")]
pub fn is_admin() -> bool {
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0 {
            let mut elevation = 0;
            let mut size = 0;
            if GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut _,
                std::mem::size_of::<i32>() as u32,
                &mut size,
            ) != 0
            {
                return elevation != 0;
            }
        }
    }
    false
}

/// Fallback for non-Windows operating systems. Always returns false.
#[cfg(not(target_os = "windows"))]
pub fn is_admin() -> bool {
    false
}
