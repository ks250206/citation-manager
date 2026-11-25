//! #22 capture composition: bounded request -> inert extraction -> #14 DOI complement ->
//! reviewable `CaptureResolution`. No library access and no PDF download happen here; the
//! caller previews `resolution.draft` through the common #33 coordinator.
use crate::{fetch_policy::FetchPolicy, html};
use cm_core::{
    ServiceError, capture as core_capture,
    import::RowDiagnostic,
    metadata_resolve::{
        Cancellation, MetadataProvider, MetadataResolution, MetadataResolveService,
    },
};

pub use cm_core::capture::*;

type Lookup<'a> = dyn FnMut(&str, &Cancellation) -> Result<MetadataResolution, ServiceError> + 'a;

/// Resolve a capture. `lookup` is the DOI provider complement (None = offline). Provider PDF
/// candidates are filtered by the same fetch policy as page candidates.
pub fn resolve_with(
    request: &CaptureRequest,
    policy: &FetchPolicy,
    lookup: Option<&mut Lookup<'_>>,
    cancel: &Cancellation,
) -> Result<CaptureResolution, ServiceError> {
    request.validate()?;
    let lookup = if request.offline { None } else { lookup };
    let page = html::extract(request, policy);
    let mut resolution = core_capture::resolve(page, lookup, cancel);
    let mut rejected = 0;
    resolution.pdf_candidates.retain(|c| {
        let ok = policy.check_url(&c.url).is_ok();
        rejected += usize::from(!ok);
        ok
    });
    if rejected > 0 && resolution.draft.diagnostics.len() < 128 {
        resolution.draft.diagnostics.push(RowDiagnostic {
            field: "pdf".into(),
            message: format!("{rejected} provider PDF link(s) not offered by the fetch policy"),
            error: false,
        });
    }
    Ok(resolution)
}

/// Convenience over a #14 resolve service (Crossref, then DataCite).
pub fn resolve<P: MetadataProvider>(
    request: &CaptureRequest,
    policy: &FetchPolicy,
    service: Option<&mut MetadataResolveService<P>>,
    cancel: &Cancellation,
) -> Result<CaptureResolution, ServiceError> {
    match service {
        Some(service) => {
            let mut lookup = |doi: &str, cancel: &Cancellation| service.resolve(doi, cancel);
            resolve_with(request, policy, Some(&mut lookup), cancel)
        }
        None => resolve_with(request, policy, None, cancel),
    }
}

#[cfg(test)]
mod tests;
