use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, Attribute, FnArg, GenericArgument, Ident, ItemTrait, LitStr, PatType,
    PathArguments, ReturnType, Type,
};

fn rpc_attribute(attributes: &[Attribute]) -> (Option<String>, bool, bool, bool) {
    let mut name = None;
    let mut notification = false;
    let mut stream = false;
    let mut reply_and_stream = false;
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
            if meta.path.is_ident("reply_and_stream") {
                reply_and_stream = true;
                return Ok(());
            }
            if meta.path.is_ident("method") {
                let value: LitStr = meta.value()?.parse()?;
                name = Some(value.value());
            }
            Ok(())
        });
    }
    (name, notification, stream, reply_and_stream)
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
            "stream RPC methods must return Result<Stream<T>, E>",
        ));
    };
    let Some(segment) = path.path.segments.last() else {
        unreachable!()
    };
    if segment.ident != "Stream" {
        return Err(syn::Error::new_spanned(
            path,
            "stream RPC methods must return Result<Stream<T>, E>",
        ));
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            segment,
            "Stream needs an item type",
        ));
    };
    match args.args.first() {
        Some(GenericArgument::Type(item)) => Ok(item.clone()),
        _ => Err(syn::Error::new_spanned(args, "Stream needs an item type")),
    }
}

fn reply_and_stream_types(output: &ReturnType) -> Result<(Type, Type), syn::Error> {
    let result = result_ok_type(output)?;
    let Type::Tuple(tuple) = result else {
        return Err(syn::Error::new_spanned(
            result,
            "reply_and_stream methods must return Result<(Reply, Stream<Item>), E>",
        ));
    };
    if tuple.elems.len() != 2 {
        return Err(syn::Error::new_spanned(
            tuple,
            "reply_and_stream needs a reply and Stream item type",
        ));
    }
    let reply = tuple.elems[0].clone();
    let stream = tuple.elems[1].clone();
    let Type::Path(path) = stream else {
        return Err(syn::Error::new_spanned(
            stream,
            "reply_and_stream needs Stream<Item>",
        ));
    };
    let segment = path.path.segments.last().unwrap();
    if segment.ident != "Stream" {
        return Err(syn::Error::new_spanned(
            path,
            "reply_and_stream needs Stream<Item>",
        ));
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            segment,
            "Stream needs an item type",
        ));
    };
    let Some(GenericArgument::Type(item)) = args.args.first() else {
        return Err(syn::Error::new_spanned(args, "Stream needs an item type"));
    };
    Ok((reply, item.clone()))
}

#[proc_macro_attribute]
pub fn interface(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(item as ItemTrait);
    let trait_ident = &input.ident;
    let handle_ident = format_ident!("{}Handle", trait_ident);
    let visibility = &input.vis;
    let mut client_methods = Vec::new();
    let mut register_methods = Vec::new();

    for method in &input.items {
        let syn::TraitItem::Fn(method) = method else {
            continue;
        };
        let method_ident = &method.sig.ident;
        let (configured_name, notification, stream, reply_and_stream) =
            rpc_attribute(&method.attrs);
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
                { let service = service.clone(); peer.register_notification(#wire_name, move |payload, peer| {
                        let service = service.clone(); Box::pin(async move {
                            let request: #request_ty = payload.decode()?;
                            #trait_ident::#method_ident(&*service, #(#call_args),*).await.map_err(::atlas_rpc::RpcError::application)
                        })
                    }); }
            });
        } else if stream || reply_and_stream {
            let (reply_ty, item_ty) = if reply_and_stream {
                match reply_and_stream_types(&method.sig.output) {
                    Ok(types) => types,
                    Err(error) => return error.to_compile_error().into(),
                }
            } else {
                match stream_item_type(&method.sig.output) {
                    Ok(item) => (syn::parse_quote! { () }, item),
                    Err(error) => return error.to_compile_error().into(),
                }
            };
            let client = if reply_and_stream {
                quote! {
                    pub async fn #method_ident(&self, #request_pat: #request_ty) -> Result<(#reply_ty, ::atlas_rpc::PeerStream<#item_ty>), ::atlas_rpc::CallError>
                    where #request_ty: ::atlas_rpc::serde::Serialize + Send + 'static, #reply_ty: ::atlas_rpc::serde::de::DeserializeOwned + Send + 'static, #item_ty: ::atlas_rpc::serde::de::DeserializeOwned + Send + 'static {
                        self.peer.reply_and_stream(#wire_name, #request_pat).await
                    }
                }
            } else {
                quote! {
                    pub fn #method_ident(&self, #request_pat: #request_ty) -> ::atlas_rpc::PeerStream<#item_ty>
                    where #request_ty: ::atlas_rpc::serde::Serialize + Send + 'static, #item_ty: ::atlas_rpc::serde::de::DeserializeOwned + Send + 'static {
                        self.peer.stream(#wire_name, #request_pat)
                    }
                }
            };
            client_methods.push(client);
            let send_reply = if reply_and_stream {
                quote! { peer.stream_item(id, reply).map_err(|_| ::atlas_rpc::RpcError::internal("stream peer closed"))?; }
            } else {
                quote! {}
            };
            let unpack = if reply_and_stream {
                quote! { let (reply, mut stream) = result; }
            } else {
                quote! { let mut stream = result; }
            };
            register_methods.push(quote! {
                { let service = service.clone(); peer.register_request(#wire_name, move |payload, peer, id| {
                        let service = service.clone(); Box::pin(async move {
                            let request: #request_ty = payload.decode()?;
                            let result = #trait_ident::#method_ident(&*service, #(#call_args),*).await.map_err(::atlas_rpc::RpcError::application)?;
                            #unpack
                            #send_reply
                            let cancelled = ::tokio_util::sync::CancellationToken::new();
                            let cancellation = cancelled.clone();
                            peer.register_cancellation(id, move || cancellation.cancel());
                            loop {
                                ::tokio::select! {
                                    _ = cancelled.cancelled() => {
                                        peer.remove_cancellation(id);
                                        return Err(::atlas_rpc::RpcError::new(-32800, "request cancelled"));
                                    }
                                    item = ::atlas_rpc::RpcStreamExt::next(&mut stream) => match item {
                                        Some(item) => peer.stream_item(id, item).map_err(|_| ::atlas_rpc::RpcError::internal("stream peer closed"))?,
                                        None => break,
                                    },
                                }
                            }
                            peer.remove_cancellation(id);
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
                { let service = service.clone(); peer.register_request(#wire_name, move |payload, peer, _id| {
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
        #visibility struct #handle_ident { peer: ::atlas_rpc::Peer }
        impl #handle_ident { pub fn new(peer: ::atlas_rpc::Peer) -> Self { Self { peer } } #(#client_methods)* }
        impl ::atlas_rpc::RpcHandle for #handle_ident { fn from_peer(peer: ::atlas_rpc::Peer) -> Self { Self::new(peer) } }

        impl<T: #trait_ident + Send + Sync + 'static> ::atlas_rpc::Service<T> for #handle_ident {
            fn register(service: T, peer: &::atlas_rpc::Peer) {
                let service = ::std::sync::Arc::new(service);
                #(#register_methods)*
            }
            fn into_handle(service: T) -> Self {
                let (caller, receiver) = ::atlas_rpc::InProcessTransport::pair();
                let caller = ::atlas_rpc::Peer::new(caller);
                let receiver = ::atlas_rpc::Peer::new(receiver);
                receiver.register::<#handle_ident, _>(service);
                #handle_ident::new(caller)
            }
        }
    })
}
