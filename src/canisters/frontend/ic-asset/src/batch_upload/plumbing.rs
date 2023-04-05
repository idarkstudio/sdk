use crate::asset::config::AssetConfig;
use crate::asset::content::Content;
use crate::asset::content_encoder::ContentEncoder;
use crate::batch_upload::semaphores::Semaphores;
use crate::canister_api::methods::chunk::create_chunk;
use crate::canister_api::types::asset::AssetDetails;

use candid::Nat;
use futures::future::try_join_all;
use futures::TryFutureExt;
use ic_utils::Canister;
use mime::Mime;
use slog::{debug, info, Logger};
use std::collections::HashMap;
use std::path::PathBuf;

const CONTENT_ENCODING_IDENTITY: &str = "identity";

// The most mb any one file is considered to have for purposes of limiting data loaded at once.
// Any file counts as at least 1 mb.
const MAX_COST_SINGLE_FILE_MB: usize = 45;

const MAX_CHUNK_SIZE: usize = 1_900_000;

#[derive(Clone, Debug)]
pub(crate) struct AssetDescriptor {
    pub(crate) source: PathBuf,
    pub(crate) key: String,
    pub(crate) config: AssetConfig,
}

pub(crate) struct ProjectAssetEncoding {
    pub(crate) chunk_ids: Vec<Nat>,
    pub(crate) sha256: Vec<u8>,
    pub(crate) already_in_place: bool,
}

pub(crate) struct ProjectAsset {
    pub(crate) asset_descriptor: AssetDescriptor,
    pub(crate) media_type: Mime,
    pub(crate) encodings: HashMap<String, ProjectAssetEncoding>,
}

pub(crate) struct ChunkUploadTarget<'a> {
    pub(crate) canister: &'a Canister<'a>,
    pub(crate) batch_id: &'a Nat,
}

#[allow(clippy::too_many_arguments)]
async fn make_project_asset_encoding(
    chunk_upload_target: Option<&ChunkUploadTarget<'_>>,
    asset_descriptor: &AssetDescriptor,
    canister_assets: &HashMap<String, AssetDetails>,
    content: &Content,
    content_encoding: &str,
    semaphores: &Semaphores,
    logger: &Logger,
) -> anyhow::Result<ProjectAssetEncoding> {
    let sha256 = content.sha256();

    let already_in_place = if let Some(canister_asset) = canister_assets.get(&asset_descriptor.key)
    {
        if canister_asset.content_type != content.media_type.to_string() {
            false
        } else if let Some(canister_asset_encoding_sha256) = canister_asset
            .encodings
            .iter()
            .find(|details| details.content_encoding == content_encoding)
            .and_then(|details| details.sha256.as_ref())
        {
            canister_asset_encoding_sha256 == &sha256
        } else {
            false
        }
    } else {
        false
    };

    let chunk_ids = if already_in_place {
        info!(
            logger,
            "  {}{} ({} bytes) sha {} is already installed",
            &asset_descriptor.key,
            content_encoding_descriptive_suffix(content_encoding),
            content.data.len(),
            hex::encode(&sha256),
        );
        vec![]
    } else if let Some(target) = chunk_upload_target {
        upload_content_chunks(
            target.canister,
            target.batch_id,
            asset_descriptor,
            content,
            &sha256,
            content_encoding,
            semaphores,
            logger,
        )
        .await?
    } else {
        vec![]
    };

    Ok(ProjectAssetEncoding {
        chunk_ids,
        sha256,
        already_in_place,
    })
}

#[allow(clippy::too_many_arguments)]
async fn make_encoding(
    chunk_upload_target: Option<&ChunkUploadTarget<'_>>,
    asset_descriptor: &AssetDescriptor,
    canister_assets: &HashMap<String, AssetDetails>,
    content: &Content,
    encoder: &Option<ContentEncoder>,
    semaphores: &Semaphores,
    logger: &Logger,
) -> anyhow::Result<Option<(String, ProjectAssetEncoding)>> {
    match encoder {
        None => {
            let identity_asset_encoding = make_project_asset_encoding(
                chunk_upload_target,
                asset_descriptor,
                canister_assets,
                content,
                CONTENT_ENCODING_IDENTITY,
                semaphores,
                logger,
            )
            .await?;
            Ok(Some((
                CONTENT_ENCODING_IDENTITY.to_string(),
                identity_asset_encoding,
            )))
        }
        Some(encoder) => {
            let encoded = content.encode(encoder)?;
            if encoded.data.len() < content.data.len() {
                let content_encoding = format!("{}", encoder);
                let project_asset_encoding = make_project_asset_encoding(
                    chunk_upload_target,
                    asset_descriptor,
                    canister_assets,
                    &encoded,
                    &content_encoding,
                    semaphores,
                    logger,
                )
                .await?;
                Ok(Some((content_encoding, project_asset_encoding)))
            } else {
                Ok(None)
            }
        }
    }
}

async fn make_encodings(
    chunk_upload_target: Option<&ChunkUploadTarget<'_>>,
    asset_descriptor: &AssetDescriptor,
    canister_assets: &HashMap<String, AssetDetails>,
    content: &Content,
    semaphores: &Semaphores,
    logger: &Logger,
) -> anyhow::Result<HashMap<String, ProjectAssetEncoding>> {
    let mut encoders = vec![None];
    for encoder in applicable_encoders(&content.media_type) {
        encoders.push(Some(encoder));
    }

    let encoding_futures: Vec<_> = encoders
        .iter()
        .map(|maybe_encoder| {
            make_encoding(
                chunk_upload_target,
                asset_descriptor,
                canister_assets,
                content,
                maybe_encoder,
                semaphores,
                logger,
            )
        })
        .collect();

    let encodings = try_join_all(encoding_futures).await?;

    let mut result: HashMap<String, ProjectAssetEncoding> = HashMap::new();

    for (key, value) in encodings.into_iter().flatten() {
        result.insert(key, value);
    }
    Ok(result)
}

async fn make_project_asset(
    chunk_upload_target: Option<&ChunkUploadTarget<'_>>,
    asset_descriptor: AssetDescriptor,
    canister_assets: &HashMap<String, AssetDetails>,
    semaphores: &Semaphores,
    logger: &Logger,
) -> anyhow::Result<ProjectAsset> {
    let file_size = std::fs::metadata(&asset_descriptor.source)?.len();
    let permits = std::cmp::max(
        1,
        std::cmp::min(
            ((file_size + 999999) / 1000000) as usize,
            MAX_COST_SINGLE_FILE_MB,
        ),
    );
    let _releaser = semaphores.file.acquire(permits).await;
    let content = Content::load(&asset_descriptor.source)?;

    let encodings = make_encodings(
        chunk_upload_target,
        &asset_descriptor,
        canister_assets,
        &content,
        semaphores,
        logger,
    )
    .await?;

    Ok(ProjectAsset {
        asset_descriptor,
        media_type: content.media_type,
        encodings,
    })
}

pub(crate) async fn make_project_assets(
    chunk_upload_target: Option<&ChunkUploadTarget<'_>>,
    asset_descriptors: Vec<AssetDescriptor>,
    canister_assets: &HashMap<String, AssetDetails>,
    logger: &Logger,
) -> anyhow::Result<HashMap<String, ProjectAsset>> {
    let semaphores = Semaphores::new();

    let project_asset_futures: Vec<_> = asset_descriptors
        .iter()
        .map(|loc| {
            make_project_asset(
                chunk_upload_target,
                loc.clone(),
                canister_assets,
                &semaphores,
                logger,
            )
        })
        .collect();
    let project_assets = try_join_all(project_asset_futures).await?;

    let mut hm = HashMap::new();
    for project_asset in project_assets {
        hm.insert(project_asset.asset_descriptor.key.clone(), project_asset);
    }
    Ok(hm)
}

async fn upload_content_chunks(
    canister: &Canister<'_>,
    batch_id: &Nat,
    asset_descriptor: &AssetDescriptor,
    content: &Content,
    sha256: &Vec<u8>,
    content_encoding: &str,
    semaphores: &Semaphores,
    logger: &Logger,
) -> anyhow::Result<Vec<Nat>> {
    if content.data.is_empty() {
        let empty = vec![];
        let chunk_id = create_chunk(canister, batch_id, &empty, semaphores).await?;
        info!(
            logger,
            "  {}{} 1/1 (0 bytes) sha {}",
            &asset_descriptor.key,
            content_encoding_descriptive_suffix(content_encoding),
            hex::encode(sha256)
        );
        return Ok(vec![chunk_id]);
    }

    let count = (content.data.len() + MAX_CHUNK_SIZE - 1) / MAX_CHUNK_SIZE;
    let chunks_futures: Vec<_> = content
        .data
        .chunks(MAX_CHUNK_SIZE)
        .enumerate()
        .map(|(i, data_chunk)| {
            create_chunk(canister, batch_id, data_chunk, semaphores).map_ok(move |chunk_id| {
                info!(
                    logger,
                    "  {}{} {}/{} ({} bytes) sha {} {}",
                    &asset_descriptor.key,
                    content_encoding_descriptive_suffix(content_encoding),
                    i + 1,
                    count,
                    data_chunk.len(),
                    hex::encode(sha256),
                    &asset_descriptor.config
                );
                debug!(logger, "{:?}", &asset_descriptor.config);

                chunk_id
            })
        })
        .collect();
    try_join_all(chunks_futures).await
}

fn content_encoding_descriptive_suffix(content_encoding: &str) -> String {
    if content_encoding == CONTENT_ENCODING_IDENTITY {
        "".to_string()
    } else {
        format!(" ({})", content_encoding)
    }
}

// todo: make this configurable https://github.com/dfinity/dx-triage/issues/152
fn applicable_encoders(media_type: &Mime) -> Vec<ContentEncoder> {
    match (media_type.type_(), media_type.subtype()) {
        (mime::TEXT, _) | (_, mime::JAVASCRIPT) | (_, mime::HTML) => vec![ContentEncoder::Gzip],
        _ => vec![],
    }
}
