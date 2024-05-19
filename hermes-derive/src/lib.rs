use proc_macro::*;
use quote::quote;

use syn::parse_macro_input;
use syn::ItemTrait;
use syn::ItemImpl;

/// Adds a .handle(self) function as well as
/// ServiceHandle which acts like a dynamic dispatch version
/// of the trait.
#[proc_macro_attribute]
pub fn service(_attrs: TokenStream, input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as ItemTrait);
    quote! (
        #ast
    ).into()
}

#[proc_macro_attribute]
pub fn implement(_attrs: TokenStream, input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as ItemImpl);
    quote! (
        #ast
    ).into()
}