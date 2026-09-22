//! Stand-in for Zed's `ztracing`.

use proc_macro::TokenStream;

/// Leaves the annotated item as written.
#[proc_macro_attribute]
pub fn instrument(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
