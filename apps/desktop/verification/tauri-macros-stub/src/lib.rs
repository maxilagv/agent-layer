/// Stand-in for the `#[tauri::command]` attribute macro.
#[proc_macro_attribute]
pub fn command(_attr: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    item
}
