use proc_macro2::*;

use convert_case::{Case, Casing};
use quote::quote;

use syn::{
    parse_macro_input, punctuated::Punctuated,
    token::{Brace, Paren}, 
    Ident, PatType, Receiver, Type,
    ReturnType, Token, Visibility
};
use syn::parse::{Parse, ParseStream, Result};

#[allow(dead_code)]
struct ServiceFn {
    interface_result: bool,
    // All must be async
    async_token: Token![async],
    fn_token: Token![fn],
    ident: Ident,
    paren_token: Paren,
    // Service functions must always have a receiver argument
    receiver: Receiver,
    sep_comma: Option<Token![,]>,
    inputs: Punctuated<PatType, Token![,]>,
    output: Type,
    semi_token: Token![;]
}

impl ServiceFn {
    fn variant_name(&self) -> String {
        self.ident.to_string().to_case(Case::Pascal)
    }

    fn input_names(&self) -> Vec<Ident> {
        self.inputs.iter().map(
            |x| {
                match x.pat.as_ref() {
                    syn::Pat::Ident(n) => n.ident.clone(),
                    _ => panic!()
                }
        }).collect()
    }

    fn trait_method(&self) -> TokenStream {
        let ident = &self.ident;
        let receiver = &self.receiver;
        let inputs = self.inputs.iter();
        let output = &self.output;
        quote! {
            async fn #ident (#receiver, #(#inputs),*) -> #output;
        }
    }

    fn request_variant(&self) -> TokenStream {
        let variant_ident = Ident::new(&self.variant_name(), self.ident.span());
        let inputs = self.inputs.iter();
        quote! {
            #variant_ident { #(#inputs),* }
        }
    }

    fn reply_variant(&self) -> TokenStream {
        let variant_ident = Ident::new(&self.variant_name(), self.ident.span());
        let output_type = &self.output;
        quote! {
            #variant_ident(#output_type)
        }
    }

    // The match branch for the dispatch()
    // Service implementation
    fn dispatch_branch(&self, req_ident: &Ident, reply_ident: &Ident) -> TokenStream {
        let ident = &self.ident;
        let variant_ident = Ident::new(&self.variant_name(), self.ident.span());
        let input_names = self.input_names();
        quote! {
            #req_ident::#variant_ident { #(#input_names),* } => 
                #reply_ident::#variant_ident(service.#ident(#(#input_names),*).await)
        }
    }

    // The interface includes the consolidated error type
    fn interface_method(&self) -> TokenStream {
        let ident = &self.ident;
        let receiver = &self.receiver;
        let inputs = self.inputs.iter();
        let output = &self.output;
        quote! {
            async fn #ident (#receiver, #(#inputs),*) -> hermes::Result<#output>;
        }
    }

    // The convenience function for the XYZHandle
    // which implements internally calls the request
    fn interface_impl(&self, req_ident: &Ident, reply_ident: &Ident) -> TokenStream {
        let ident = &self.ident;
        let variant_ident = Ident::new(&self.variant_name(), self.ident.span());

        let receiver = &self.receiver;
        let inputs = self.inputs.iter();
        let input_names = self.input_names();
        let output = &self.output;

        // Extract the type from output and replace by () if default

        // Will generate fn foo() and fn foo_ext() methods
        // for the *Client* type
        quote! {
            async fn #ident (#receiver, #(#inputs),*) 
                        -> hermes::Result<#output> {
                let __req = #req_ident::#variant_ident { #(#input_names),* };
                Ok(match self.dispatch(__req).await? {
                #reply_ident::#variant_ident(val) => val,
                _ => panic!()
                })
            }
        }
    }
}

impl Parse for ServiceFn {
    fn parse(input: ParseStream) -> Result<Self> {
        // let attrs : Vec<Attribute> = input.call(Attribute::parse_outer)?;
        let async_token= input.parse()?;
        let fn_token = input.parse()?;
        let ident = input.parse()?;

        let arg_content;
        let paren_token = syn::parenthesized!(arg_content in input);

        let receiver = arg_content.parse()?;
        let sep_comma = arg_content.parse()?;

        let mut inputs = Punctuated::new();
        while !arg_content.is_empty() {
            let arg = arg_content.parse()?;
            inputs.push(arg);
            if arg_content.is_empty() {
                break;
            }
            inputs.push_punct(arg_content.parse()?)
        }

        let output : ReturnType = input.parse()?;
        let output = match output {
            ReturnType::Default => Type::Verbatim(quote! {()}),
            ReturnType::Type(_, t) => *t
        };
        let semi_token = input.parse()?;
        Ok(ServiceFn {
            interface_result: false, 
            async_token, fn_token,
            ident, paren_token,
            receiver, sep_comma,
            inputs, output,
            semi_token
        })
    }
}

#[allow(dead_code)]
struct ServiceDefinition {
    visibility: Visibility,
    trait_token: Token![trait],
    ident: Ident,
    error_type: Type,
    brace_token: Brace,
    methods: Vec<ServiceFn>
}

impl ServiceDefinition {
    fn trait_definiton(&self) -> TokenStream {
        let visibility = &self.visibility;
        let ident = &self.ident;

        let trait_methods: Vec<_> = self.methods.iter().map(|x| x.trait_method()).collect();
        quote! {
            #[hermes::async_trait(?Send)]
            #visibility trait #ident {
                #(#trait_methods)*
            }
        }
    }

    fn interface_definition(&self) -> TokenStream {
        let visibility = &self.visibility;
        let req_ident = Ident::new(&format!("{}Request", self.ident), self.ident.span());
        let reply_ident = Ident::new(&format!("{}Reply", self.ident), self.ident.span());

        let interface_ident = Ident::new(&format!("{}Interface", self.ident), self.ident.span());
        let interface_methods: Vec<_> = self.methods.iter().map(|x| x.interface_method()).collect();
        let interface_impl: Vec<_> = self.methods.iter().map(|x| x.interface_impl(&req_ident, &reply_ident)).collect();

        quote! {
            #visibility trait #interface_ident {
                #(#interface_methods)*
            }
            impl<S> #interface_ident for S where S: hermes::Service<#req_ident> {
                #(#interface_impl)*
            }
        }
    }

    fn enum_definitions(&self) -> TokenStream {
        let ident = &self.ident;
        let req_ident = Ident::new(&format!("{}Request", self.ident), self.ident.span());
        let reply_ident = Ident::new(&format!("{}Reply", self.ident), self.ident.span());

        let visibility = &self.visibility;
        let req_variants : Vec<_> = self.methods.iter().map(ServiceFn::request_variant).collect();
        let reply_variants : Vec<_> = self.methods.iter().map(ServiceFn::reply_variant).collect();

        let dispatch_branches: Vec<_> = self.methods.iter().map(
            |x| x.dispatch_branch(&req_ident, &reply_ident)
        ).collect();
        quote! {
            #visibility enum #req_ident {
                #(#req_variants),*
            }
            #visibility enum #reply_ident {
                #(#reply_variants),*
            }
            impl hermes::Request for #req_ident {
                type Reply = #reply_ident;
            }

            #[hermes::async_trait(?Send)]
            impl<S> hermes::DispatchInto<S> for #req_ident where S : #ident {
                async fn dispatch_into(self, service: &S) 
                        -> hermes::Result<#reply_ident> {
                    Ok(match self {
                        #(#dispatch_branches),*
                    })
                }
            }
        }
    }

    fn handle_definition(&self) -> TokenStream {
        let visibility = &self.visibility;
        let handle_ident = Ident::new(&format!("{}Handle", self.ident), self.ident.span());
        let req_ident = Ident::new(&format!("{}Request", self.ident), self.ident.span());
        quote! {
            #visibility type #handle_ident = hermes::Handle<#req_ident>;
        }
    }
}

impl Parse for ServiceDefinition {
    fn parse(input: ParseStream) -> Result<Self> {
        let visibility = input.parse()?;
        let trait_token = input.parse()?;
        let ident = input.parse()?;

        let content;
        let brace_token = syn::braced!(content in input);

        let error_type = syn::Type::Verbatim(quote! {hermes::Error});

        let mut methods = Vec::new();
        while !content.is_empty() {
            methods.push(content.parse()?);
        }
        Ok(ServiceDefinition {
            visibility, trait_token,
            ident, error_type, brace_token, methods
        })
    }
}

/// Adds a .dispatch() function as well as
/// associated response, request types and code
/// for automatic marshalling/unmarshalling
#[proc_macro_attribute]
pub fn service(_attrs: proc_macro::TokenStream, input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let def = parse_macro_input!(input as ServiceDefinition);
    let trait_def = def.trait_definiton();
    let interface_def= def.interface_definition();
    let enum_defs = def.enum_definitions();
    let handle_def = def.handle_definition();
    // let client_def = def.client_definition();
    let output = quote! {
        #enum_defs
        #trait_def
        #interface_def
        #handle_def
    };
    // eprintln!("{}", pretty(&output));
    output.into()
}

#[proc_macro_attribute]
pub fn implement(_attrs: proc_macro::TokenStream,
                 input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: TokenStream = input.into();
    quote! {
        #[hermes::async_trait(?Send)]
        #input
    }.into()
}

#[allow(dead_code)]
fn pretty(file: &proc_macro2::TokenStream) -> String {
    let parsed : syn::Result<syn::File> = syn::parse2(file.clone());
    if let Ok(file) = parsed {
        prettyplease::unparse(&file)
    } else {
        return format!("{}", file)
    }
}