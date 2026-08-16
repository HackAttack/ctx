//! Index-free bindings shared by ctx history provider packs.
//!
//! Provider parsers and projectors use this crate's static runtime profile.
//! Capture remains responsible for concrete index lifecycle, deferred record
//! storage, route identity, discovery, registration policy, and publication.

mod adapter;
mod error;
mod jsonl;
mod record;
mod route;
pub mod source_io;
mod sqlite;

pub use adapter::*;
pub use error::*;
pub use jsonl::*;
pub use record::*;
pub use route::*;
pub use sqlite::*;

use std::marker::PhantomData;

use ctx_history_capture_runtime::{
    BaseEventLookup, CaptureLifecycleSink, ChangedDocumentSink, DocumentAppendBase,
    DocumentBaseRoute, DocumentRecordSpool, ReplacementDocumentTree, SourceBackedRouteDriver,
};
use ctx_history_jsonl::{JsonlFamilyRuntime, JsonlRuntimeDriver};

/// Compile-time runtime profile supplied by the composing capture façade.
///
/// Keeping both concrete associated types above this crate prevents provider
/// packs from acquiring index or publication authority.
pub trait ProviderRuntimeBinding: Send + Sync + 'static {
    type CaptureLifecycleSink: CaptureLifecycleSink;
    type DocumentRecordSpool: DocumentRecordSpool;
}

pub type ProviderBaseEventLookup<B> =
    <<B as ProviderRuntimeBinding>::CaptureLifecycleSink as CaptureLifecycleSink>::BaseLookup;
pub type ProviderRouteDriver<B> = SourceBackedRouteDriver<
    <B as ProviderRuntimeBinding>::CaptureLifecycleSink,
    ProviderRouteControlExpectation,
>;
pub type ProviderChangedDocumentSink<'sink, 'writer, B> = ChangedDocumentSink<
    'sink,
    'writer,
    <B as ProviderRuntimeBinding>::CaptureLifecycleSink,
    <B as ProviderRuntimeBinding>::DocumentRecordSpool,
>;
pub type ProviderDocumentAppendBase<B> =
    DocumentAppendBase<<B as ProviderRuntimeBinding>::CaptureLifecycleSink>;
pub type ProviderDocumentBaseRoute<'scan, 'writer, B> =
    DocumentBaseRoute<'scan, 'writer, <B as ProviderRuntimeBinding>::CaptureLifecycleSink>;

/// Shared JSONL family profile for one provider runtime binding.
pub struct ProviderJsonlRuntime<B>(PhantomData<fn() -> B>);

impl<B: ProviderRuntimeBinding> JsonlFamilyRuntime for ProviderJsonlRuntime<B> {
    type Error = CaptureError;
    type Lifecycle = B::CaptureLifecycleSink;
    type WorkerServices = ();
    type RouteControl = ProviderRouteControlExpectation;

    fn begin_worker_leaf(_services: &mut Self::WorkerServices) {}
}

pub type ProviderJsonlRouteDriver<B> = JsonlRuntimeDriver<ProviderJsonlRuntime<B>>;
pub type ProviderFallbackEventIdentityState<B> =
    ctx_history_jsonl::FallbackEventIdentityState<ProviderBaseEventLookup<B>, CaptureError>;
pub type ProviderBaseEventLookupError<B> = <ProviderBaseEventLookup<B> as BaseEventLookup>::Error;

/// Marker for a document adapter bound to exactly one provider runtime.
pub trait ProviderReplacementDocumentTree<B: ProviderRuntimeBinding>:
    ReplacementDocumentTree<
    Lifecycle = B::CaptureLifecycleSink,
    Spool = B::DocumentRecordSpool,
    RouteControl = ProviderRouteControlExpectation,
>
{
}

impl<B, A> ProviderReplacementDocumentTree<B> for A
where
    B: ProviderRuntimeBinding,
    A: ReplacementDocumentTree<
        Lifecycle = B::CaptureLifecycleSink,
        Spool = B::DocumentRecordSpool,
        RouteControl = ProviderRouteControlExpectation,
    >,
{
}
