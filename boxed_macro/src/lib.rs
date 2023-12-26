extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::DeriveInput;

#[proc_macro_derive(Boxed)]
pub fn boxed_macro_derive(input: TokenStream) -> TokenStream {
    let ast: DeriveInput = syn::parse(input).unwrap();
    impl_boxed_macro(&ast)
}

fn impl_boxed_macro(ast: &syn::DeriveInput) -> TokenStream {
    let name = &ast.ident;
    let gen = quote! {
        impl BoxedDefault for #name {
            fn boxed() -> Box<dyn LineBuilder> {
                Box::new(#name::default())
            }
        }
    };
    gen.into()
}
