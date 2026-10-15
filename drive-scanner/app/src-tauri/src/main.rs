// No console window in the release build (Ravi's rule: no black terminal windows).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    avni_drive_scanner_lib::run()
}
