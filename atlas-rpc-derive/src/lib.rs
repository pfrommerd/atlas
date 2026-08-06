use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, Attribute, FnArg, GenericArgument, Ident, ItemTrait, LitStr, PatType,
    PathArguments, ReturnType, Type,
};

fn rpc_attribute(attributes: &[Attribute]) -> (Option<String>, bool, bool) {
    let mut name = None;
    let mut notification = false;
    let mut stream = false;
    for attribute in attributes {
        if !attribute.path().is_ident("rpc") {
            continue;
        }
        let _ = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("notification") {
                notification = true;
                return Ok(());
            }
            if meta.path.is_ident("stream") {
                stream = true;
                return Ok(());
            }
            if meta.path.is_ident("method") {
                let value: LitStr = meta.value()?.parse()?;
                name = Some(value.value());
            }
            Ok(())
        });
    }
    (name, notification, stream)
}

fn is_context(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("rpc")
            && attribute
                .parse_args::<Ident>()
                .map(|ident| ident == "context")
                .unwrap_or(false)
    })
}

fn result_ok_type(output: &ReturnType) -> Result<Type, syn::Error> {
    let ReturnType::Type(_, ty) = output else {
        return Err(syn::Error::new_spanned(
            output,
            "request RPC methods must return Result<T, E>",
        ));
    };
    let Type::Path(path) = &**ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "request RPC methods must return Result<T, E>",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        unreachable!()
    };
    if segment.ident != "Result" {
        return Err(syn::Error::new_spanned(
            ty,
            "request RPC methods must return Result<T, E>",
        ));
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "request RPC methods must return Result<T, E>",
        ));
    };
    match args.args.first() {
        Some(GenericArgument::Type(ok)) => Ok(ok.clone()),
        _ => Err(syn::Error::new_spanned(ty, "Result needs a success type")),
    }
}

fn stream_item_type(output: &ReturnType) -> Result<Type, syn::Error> {
    let result = result_ok_type(output)?;
    let Type::Path(path) = result else {
        return Err(syn::Error::new_spanned(
            result,
            "stream RPC methods must return Result<ServerStream<T>, E>",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        unreachable!()
    };
    if segment.ident != "ServerStream" {
        return Err(syn::Error::new_spanned(
            path,
            "stream RPC methods must return Result<ServerStream<T>, E>",
        ));
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            segment,
            "ServerStream needs an item type",
        ));
    };
    match args.args.first() {
        Some(GenericArgument::Type(item)) => Ok(item.clone()),
        _ => Err(syn::Error::new_spanned(
            args,
            "ServerStream needs an item type",
        )),
    }
}

#[proc_macro_attribute]
pub fn interface(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemTrait);
    let trait_ident = &input.ident;
    let client_ident = format_ident!("{}Client", trait_ident);
    let server_ident = format_ident!("{}Server", trait_ident);
    let visibility = &input.vis;
    let mut client_methods = Vec::new();
    let mut register_methods = Vec::new();

    for method in &input.items {
        let syn::TraitItem::Fn(method) = method else {
            continue;
        };
        let method_ident = &method.sig.ident;
        let (configured_name, notification, stream) = rpc_attribute(&method.attrs);
        let wire_name = configured_name.unwrap_or_else(|| method_ident.to_string());
        let mut args = method.sig.inputs.iter();
        if !matches!(args.next(), Some(FnArg::Receiver(_))) {
            return syn::Error::new_spanned(&method.sig, "RPC methods must take &self")
                .to_compile_error()
                .into();
        }
        let mut context_ty: Option<Type> = None;
        let mut request: Option<&PatType> = None;
        for argument in args {
            let FnArg::Typed(argument) = argument else {
                continue;
            };
            if is_context(&argument.attrs) {
                context_ty = Some((*argument.ty).clone());
            } else if request.replace(argument).is_some() {
                return syn::Error::new_spanned(
                    argument,
                    "RPC methods take one request value plus an optional #[rpc(context)] value",
                )
                .to_compile_error()
                .into();
            }
        }
        let Some(request) = request else {
            return syn::Error::new_spanned(&method.sig, "RPC methods need one request value")
                .to_compile_error()
                .into();
        };
        let request_pat = &request.pat;
        let request_ty = &request.ty;
        let call_args: Vec<_> = if context_ty.is_some() {
            vec![
                quote! { ::atlas_rpc::RpcContext::<_>::from_peer(peer.clone()) },
                quote! { request },
            ]
        } else {
            vec![quote! { request }]
        };
        if notification {
            client_methods.push(quote! {
                pub fn #method_ident(&self, #request_pat: #request_ty) -> Result<(), ::atlas_rpc::CallError>
                where #request_ty: ::atlas_rpc::serde::Serialize + Send + 'static {
                    self.peer.notify(#wire_name, #request_pat)
                }
            });
            register_methods.push(quote! {
                { let service = self.service.clone(); peer.register_notification(#wire_name, move |payload, peer| {
                        let service = service.clone(); Box::pin(async move {
                            let request: #request_ty = payload.decode()?;
                            #trait_ident::#method_ident(&*service, #(#call_args),*).await.map_err(::atlas_rpc::RpcError::application)
                        })
                    }); }
            });
        } else if stream {
            let item_ty = match stream_item_type(&method.sig.output) {
                Ok(ty) => ty,
                Err(error) => return error.to_compile_error().into(),
            };
            client_methods.push(quote! {
                pub fn #method_ident(&self, #request_pat: #request_ty) -> ::atlas_rpc::ClientStream<#item_ty>
                where #request_ty: ::atlas_rpc::serde::Serialize + Send + 'static, #item_ty: ::atlas_rpc::serde::de::DeserializeOwned + Send + 'static {
                    self.peer.stream(#wire_name, #request_pat)
                }
            });
            register_methods.push(quote! {
                { let service = self.service.clone(); peer.register_request(#wire_name, move |payload, peer, id| {
                        let service = service.clone(); Box::pin(async move {
                            let request: #request_ty = payload.decode()?;
                            let mut stream = #trait_ident::#method_ident(&*service, #(#call_args),*).await.map_err(::atlas_rpc::RpcError::application)?;
                            while let Some(item) = ::atlas_rpc::RpcStreamExt::next(&mut stream).await { peer.stream_item(id, item).map_err(|_| ::atlas_rpc::RpcError::internal("stream peer closed"))?; }
                            Ok(::atlas_rpc::Payload::new(()))
                        })
                    }); }
            });
        } else {
            let response_ty = match result_ok_type(&method.sig.output) {
                Ok(ty) => ty,
                Err(error) => return error.to_compile_error().into(),
            };
            client_methods.push(quote! {
                pub async fn #method_ident(&self, #request_pat: #request_ty) -> Result<#response_ty, ::atlas_rpc::CallError>
                where #request_ty: ::atlas_rpc::serde::Serialize + Send + 'static {
                    self.peer.call(#wire_name, #request_pat).await
                }
            });
            register_methods.push(quote! {
                { let service = self.service.clone(); peer.register_request(#wire_name, move |payload, peer, _id| {
                        let service = service.clone(); Box::pin(async move {
                            let request: #request_ty = payload.decode()?;
                            let result = #trait_ident::#method_ident(&*service, #(#call_args),*).await.map_err(::atlas_rpc::RpcError::application)?;
                            Ok(::atlas_rpc::Payload::new(result))
                        })
                    }); }
            });
        }
    }
    for item in &mut input.items {
        let syn::TraitItem::Fn(method) = item else {
            continue;
        };
        method
            .attrs
            .retain(|attribute| !attribute.path().is_ident("rpc"));
        for argument in &mut method.sig.inputs {
            if let FnArg::Typed(argument) = argument {
                argument
                    .attrs
                    .retain(|attribute| !attribute.path().is_ident("rpc"));
            }
        }
        if method.sig.asyncness.take().is_some() {
            let output = match &method.sig.output {
                ReturnType::Default => quote! { () },
                ReturnType::Type(_, ty) => quote! { #ty },
            };
            method.sig.output =
                syn::parse_quote! { -> impl ::core::future::Future<Output = #output> + Send };
        }
    }
    TokenStream::from(quote! {
        #input

        #[derive(Clone)]
        #visibility struct #client_ident { peer: ::atlas_rpc::Peer }
        impl #client_ident { pub fn new(peer: ::atlas_rpc::Peer) -> Self { Self { peer } } #(#client_methods)* }
        impl ::atlas_rpc::RpcClient for #client_ident { fn from_peer(peer: ::atlas_rpc::Peer) -> Self { Self::new(peer) } }

        #visibility struct #server_ident<T: #trait_ident + Send + Sync + 'static> { service: ::std::sync::Arc<T> }
        impl<T: #trait_ident + Send + Sync + 'static> #server_ident<T> {
            pub fn new(service: T) -> Self { Self { service: ::std::sync::Arc::new(service) } }
            pub fn register(&self, peer: &::atlas_rpc::Peer) {
                let service = self.service.clone();
                #(#register_methods)*
            }
        }
    })
}
