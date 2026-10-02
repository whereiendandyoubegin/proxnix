use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens, TokenTree};
use quote::{ToTokens, quote};
use syn::spanned::Spanned;
use syn::{FnArg, ImplItem, Item, ItemImpl, ReturnType, Signature, TraitItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forbidden {
    Mutation,
    Unsafe,
    Effect,
    Logging,
    Rendering,
    InteriorMutability,
    StringSignature,
}

impl Forbidden {
    fn message(self) -> &'static str {
        match self {
            Forbidden::Mutation => "pure_only: `mut` is not allowed; return a new value instead",
            Forbidden::Unsafe => "pure_only: `unsafe` is not allowed",
            Forbidden::Effect => "pure_only: this reaches the outside world (process, filesystem, sockets, environment, threads, clock or an effectful crate)",
            Forbidden::Logging => "pure_only: logging and printing are effects; return a report instead",
            Forbidden::Rendering => "pure_only: rendering to text is not allowed in pure code",
            Forbidden::InteriorMutability => "pure_only: interior mutability is not allowed",
            Forbidden::StringSignature => "pure_only: strings are not allowed in signatures; take and return typed values",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Violation {
    span: Span,
    rule: Forbidden,
}

fn leaves(stream: Tokens) -> Vec<TokenTree> {
    stream
        .into_iter()
        .flat_map(|tree| match tree {
            TokenTree::Group(group) => leaves(group.stream()),
            leaf => vec![leaf],
        })
        .collect()
}

fn ident_rule(name: &str) -> Option<Forbidden> {
    match name {
        "mut" => Some(Forbidden::Mutation),
        "unsafe" => Some(Forbidden::Unsafe),
        "Command" | "Child" | "Stdio" | "File" | "OpenOptions" | "TcpStream" | "TcpListener" | "UdpSocket"
        | "Instant" | "SystemTime" | "process" | "fs" | "env" | "thread" | "io" | "tokio"
        | "reqwest" | "proxmox_api" | "sozu_command_lib" | "git2" | "rayon" => Some(Forbidden::Effect),
        "tracing" => Some(Forbidden::Logging),
        "Cell" | "RefCell" | "UnsafeCell" | "Mutex" | "RwLock" | "OnceCell" | "OnceLock" | "LazyCell" | "LazyLock" => {
            Some(Forbidden::InteriorMutability)
        }
        atomic if atomic.starts_with("Atomic") => Some(Forbidden::InteriorMutability),
        "to_string" => Some(Forbidden::Rendering),
        _ => None,
    }
}

fn macro_rule(name: &str) -> Option<Forbidden> {
    match name {
        "println" | "print" | "eprintln" | "eprint" | "dbg" | "info" | "warn" | "error" | "debug" | "trace" => {
            Some(Forbidden::Logging)
        }
        "format" | "write" | "writeln" | "format_args" => Some(Forbidden::Rendering),
        _ => None,
    }
}

fn token_violations(tokens: &[TokenTree]) -> Vec<Violation> {
    let idents = tokens.iter().filter_map(|token| match token {
        TokenTree::Ident(ident) => ident_rule(&ident.to_string()).map(|rule| Violation { span: ident.span(), rule }),
        _ => None,
    });
    let pairs = tokens.windows(2).filter_map(|pair| match pair {
        [TokenTree::Ident(ident), TokenTree::Punct(punct)] if punct.as_char() == '!' => {
            macro_rule(&ident.to_string()).map(|rule| Violation { span: ident.span(), rule })
        }
        [TokenTree::Ident(ident), TokenTree::Punct(punct)] if punct.as_char() == ':' && ident == "log" => {
            Some(Violation { span: ident.span(), rule: Forbidden::Logging })
        }
        _ => None,
    });
    idents.chain(pairs).collect()
}

fn mentions_string(tokens: Tokens) -> bool {
    leaves(tokens).iter().any(|token| matches!(token, TokenTree::Ident(ident) if ident == "str" || ident == "String"))
}

fn signature_violations(signature: &Signature) -> Vec<Violation> {
    let inputs = signature.inputs.iter().filter_map(|input| match input {
        FnArg::Typed(typed) => mentions_string(typed.ty.to_token_stream()).then(|| typed.ty.span()),
        FnArg::Receiver(_) => None,
    });
    let output = match &signature.output {
        ReturnType::Type(_, ty) => mentions_string(ty.to_token_stream()).then(|| ty.span()),
        ReturnType::Default => None,
    };
    inputs
        .chain(output)
        .map(|span| Violation { span, rule: Forbidden::StringSignature })
        .collect()
}

fn trait_name(implementation: &ItemImpl) -> Option<(String, Tokens)> {
    implementation.trait_.as_ref().and_then(|(_, path, _)| {
        path.segments
            .last()
            .map(|segment| (segment.ident.to_string(), segment.arguments.to_token_stream()))
    })
}

fn converts_text(implementation: &ItemImpl) -> bool {
    match trait_name(implementation) {
        Some((name, _)) if name == "FromStr" => true,
        Some((name, arguments)) if name == "From" || name == "TryFrom" || name == "AsRef" => mentions_string(arguments),
        _ => false,
    }
}

fn renders_text(implementation: &ItemImpl) -> bool {
    matches!(trait_name(implementation), Some((name, _)) if name == "Display" || name == "Debug")
}

fn impl_violations(implementation: &ItemImpl) -> Vec<Violation> {
    let rendering = renders_text(implementation)
        .then(|| Violation { span: implementation.span(), rule: Forbidden::Rendering });
    let signatures = implementation
        .items
        .iter()
        .filter_map(|item| match item {
            ImplItem::Fn(function) if !converts_text(implementation) => Some(signature_violations(&function.sig)),
            _ => None,
        })
        .flatten();
    rendering.into_iter().chain(signatures).collect()
}

fn item_violations(item: &Item) -> Vec<Violation> {
    match item {
        Item::Fn(function) => signature_violations(&function.sig),
        Item::Impl(implementation) => impl_violations(implementation),
        Item::Trait(definition) => definition
            .items
            .iter()
            .filter_map(|item| match item {
                TraitItem::Fn(function) => Some(signature_violations(&function.sig)),
                _ => None,
            })
            .flatten()
            .collect(),
        _ => Vec::new(),
    }
}

fn check(item: &Item, tokens: Tokens) -> Vec<Violation> {
    token_violations(&leaves(tokens))
        .into_iter()
        .chain(item_violations(item))
        .collect()
}

#[proc_macro_attribute]
pub fn pure_only(attribute: TokenStream, input: TokenStream) -> TokenStream {
    let tokens = Tokens::from(input);
    let arguments = Tokens::from(attribute);
    match syn::parse2::<Item>(tokens.clone()) {
        Err(error) => error.to_compile_error().into(),
        Ok(_) if !arguments.is_empty() => {
            syn::Error::new(arguments.span(), "pure_only takes no arguments").to_compile_error().into()
        }
        Ok(item) => {
            let errors: Tokens = check(&item, tokens.clone())
                .into_iter()
                .map(|violation| syn::Error::new(violation.span, violation.rule.message()).to_compile_error())
                .collect();
            quote!(#errors #tokens).into()
        }
    }
}
