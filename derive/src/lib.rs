//! Derive macros for [`octoevents`](https://docs.rs/octoevents).
//!
//! Reached through `octoevents`'s `derive` feature, on by default, which
//! re-exports everything here; there is no reason to depend on this crate
//! directly. The two crates are released together and `octoevents` pins this
//! one exactly, because what the derive expands to is an `impl` of an
//! `octoevents` trait.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{DeriveInput, Error, Expr, Meta, Result, parse_macro_input, parse_quote};

/// Declares which `EventKind` a serde type is the payload of.
///
/// The attribute takes the kind as an expression, `#[payload(EventKind::..)]`,
/// and expands to `impl octoevents::Payload for Self { const KIND = .. }`,
/// carrying the type's own generics and bounds and adding one:
/// `Self: serde::de::DeserializeOwned`. Nothing else is generated: the serde
/// derive stays yours, and so does every field. The added bound is what
/// makes a serde type a `FromEnvelope`, which `Payload` requires, so a
/// generic view `View<T>` is a payload wherever `View<T>` deserializes, with
/// nothing said about `T` beyond what the type itself declares. A misspelled
/// variant is reported by rustc at the path, as any expression would be.
///
/// The attribute is required, exactly once, and takes the kind
/// positionally: `#[payload]` and `#[payload = ".."]` are refused with the
/// parenthesized form, `#[payload()]` as empty, a second attribute as a
/// duplicate, and a second argument as an unexpected token. The kind is any
/// expression of type `EventKind`, a path (`EventKind::Issues`,
/// `octoevents::EventKind::Issues`) or a constant of your own.
///
/// The expansion names `::serde` and `::octoevents`, so both crates are
/// expected under those names, as they are wherever `serde::Deserialize` is
/// derived and `octoevents` is depended on.
///
/// ```
/// use octoevents::{EventKind, Payload};
///
/// #[derive(serde::Deserialize, Payload)]
/// #[payload(EventKind::Issues)]
/// struct IssueOpened {
///     issue: Issue,
/// }
///
/// #[derive(serde::Deserialize)]
/// struct Issue {
///     number: u64,
/// }
///
/// assert_eq!(IssueOpened::KIND, EventKind::Issues);
/// ```
#[proc_macro_derive(Payload, attributes(payload))]
pub fn derive_payload(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_payload(&input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// The `impl Payload` for one type, or the error the attribute earned.
///
/// The impl carries the type's own generics and bounds, plus `Self:
/// DeserializeOwned`. `Payload` requires `FromEnvelope`, which a serde type
/// has through the blanket impl over `Payload + DeserializeOwned`; without
/// the bound spelled on the impl, a generic view `View<T>` would owe
/// `FromEnvelope` for every `T`, including the ones that do not deserialize,
/// and the impl would be refused. With it, `View<T>` is a payload wherever
/// `View<T>` deserializes, and nothing further is said about `T`.
fn expand_payload(input: &DeriveInput) -> Result<TokenStream> {
    let kind = declared_kind(input)?;
    let ident = &input.ident;
    let (_, ty_generics, _) = input.generics.split_for_impl();
    let mut generics = input.generics.clone();
    generics
        .make_where_clause()
        .predicates
        .push(parse_quote!(#ident #ty_generics: ::serde::de::DeserializeOwned));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::octoevents::Payload for #ident #ty_generics #where_clause {
            const KIND: ::octoevents::EventKind = #kind;
        }
    })
}

/// The one `#[payload(..)]` attribute's kind expression.
///
/// Exactly one attribute is required; none, or a second, is an error at the
/// type name or at the extra attribute.
fn declared_kind(input: &DeriveInput) -> Result<Expr> {
    let mut kind = None;
    for attr in input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("payload"))
    {
        if kind.is_some() {
            return Err(Error::new_spanned(
                attr,
                "duplicate `#[payload]` attribute: a payload declares one kind",
            ));
        }
        match &attr.meta {
            Meta::List(_) => {}
            Meta::Path(_) | Meta::NameValue(_) => {
                return Err(Error::new_spanned(
                    attr,
                    "`#[payload]` takes the event kind in parentheses: `#[payload(EventKind::..)]`",
                ));
            }
        }
        kind = Some(attr.parse_args::<KindArg>()?.0);
    }
    kind.ok_or_else(|| {
        Error::new(
            input.ident.span(),
            "missing `#[payload(EventKind::..)]`: a payload declares the kind it decodes",
        )
    })
}

/// The contents of `#[payload(..)]`: the kind expression.
///
/// Positional and nothing else. Should the attribute ever carry a second
/// datum, a keyed one can be added after the kind (`#[payload(EventKind::..,
/// other = ..)]`) without this form having to go, so no keyed spelling is
/// reserved for the kind itself.
struct KindArg(Expr);

impl Parse for KindArg {
    fn parse(input: ParseStream) -> Result<Self> {
        if input.is_empty() {
            return Err(Error::new(
                input.span(),
                "`#[payload(..)]` is empty: it takes the event kind, `#[payload(EventKind::..)]`",
            ));
        }
        Ok(Self(input.parse()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    /// The expansion rendered through `TokenStream::to_string`, the same
    /// path an expected stream written with `quote!` is rendered through, so
    /// a positive test compares two renderings of proc-macro2's making and
    /// never a hand-spaced string: how proc-macro2 spaces tokens is not a
    /// contract of that crate.
    fn expand(input: &DeriveInput) -> Result<String> {
        expand_payload(input).map(|tokens| tokens.to_string())
    }

    #[test]
    fn positional_kind_expands_to_the_impl() {
        let tokens = expand(&parse_quote! {
            #[payload(EventKind::Issues)]
            struct IssueOpened { number: u64 }
        })
        .unwrap();

        assert_eq!(
            tokens,
            quote! {
                #[automatically_derived]
                impl ::octoevents::Payload for IssueOpened
                where
                    IssueOpened: ::serde::de::DeserializeOwned
                {
                    const KIND: ::octoevents::EventKind = EventKind::Issues;
                }
            }
            .to_string()
        );
    }

    #[test]
    fn any_expression_is_accepted_as_the_kind() {
        let tokens = expand(&parse_quote! {
            #[payload(octoevents::EventKind::PullRequest)]
            struct PullRequestNumber;
        })
        .unwrap();

        assert_eq!(
            tokens,
            quote! {
                #[automatically_derived]
                impl ::octoevents::Payload for PullRequestNumber
                where
                    PullRequestNumber: ::serde::de::DeserializeOwned
                {
                    const KIND: ::octoevents::EventKind = octoevents::EventKind::PullRequest;
                }
            }
            .to_string()
        );
    }

    #[test]
    fn generics_are_carried_onto_the_impl_beside_the_serde_bound() {
        let tokens = expand(&parse_quote! {
            #[payload(EventKind::Issues)]
            struct View<T: Clone> where T: Send { inner: T }
        })
        .unwrap();

        assert_eq!(
            tokens,
            quote! {
                #[automatically_derived]
                impl<T: Clone> ::octoevents::Payload for View<T>
                where
                    T: Send,
                    View<T>: ::serde::de::DeserializeOwned
                {
                    const KIND: ::octoevents::EventKind = EventKind::Issues;
                }
            }
            .to_string()
        );
    }

    #[test]
    fn a_type_without_a_where_clause_gets_one_for_the_serde_bound() {
        let tokens = expand(&parse_quote! {
            #[payload(EventKind::Issues)]
            struct View<T> { inner: T }
        })
        .unwrap();

        assert_eq!(
            tokens,
            quote! {
                #[automatically_derived]
                impl<T> ::octoevents::Payload for View<T>
                where
                    View<T>: ::serde::de::DeserializeOwned
                {
                    const KIND: ::octoevents::EventKind = EventKind::Issues;
                }
            }
            .to_string()
        );
    }

    #[test]
    fn other_attributes_are_ignored() {
        let alone = expand(&parse_quote! {
            #[payload(EventKind::Issues)]
            struct IssueOpened;
        })
        .unwrap();
        let among_others = expand(&parse_quote! {
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "snake_case")]
            #[payload(EventKind::Issues)]
            struct IssueOpened;
        })
        .unwrap();

        assert_eq!(among_others, alone);
    }

    #[test]
    fn missing_attribute_is_reported_at_the_type() {
        let err = expand(&parse_quote! {
            struct IssueOpened;
        })
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "missing `#[payload(EventKind::..)]`: a payload declares the kind it decodes"
        );
    }

    #[test]
    fn a_second_attribute_is_a_duplicate() {
        let err = expand(&parse_quote! {
            #[payload(EventKind::Issues)]
            #[payload(EventKind::IssueComment)]
            struct IssueOpened;
        })
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "duplicate `#[payload]` attribute: a payload declares one kind"
        );
    }

    #[test]
    fn bare_attribute_asks_for_parentheses() {
        let err = expand(&parse_quote! {
            #[payload]
            struct IssueOpened;
        })
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "`#[payload]` takes the event kind in parentheses: `#[payload(EventKind::..)]`"
        );
    }

    #[test]
    fn name_value_attribute_asks_for_parentheses() {
        let err = expand(&parse_quote! {
            #[payload = "issues"]
            struct IssueOpened;
        })
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "`#[payload]` takes the event kind in parentheses: `#[payload(EventKind::..)]`"
        );
    }

    #[test]
    fn empty_parentheses_are_reported() {
        let err = expand(&parse_quote! {
            #[payload()]
            struct IssueOpened;
        })
        .unwrap_err();

        assert_eq!(
            err.to_string(),
            "`#[payload(..)]` is empty: it takes the event kind, `#[payload(EventKind::..)]`"
        );
    }

    #[test]
    fn trailing_tokens_are_refused() {
        let err = expand(&parse_quote! {
            #[payload(EventKind::Issues, EventKind::IssueComment)]
            struct IssueOpened;
        })
        .unwrap_err();

        assert_eq!(err.to_string(), "unexpected token");
    }
}
