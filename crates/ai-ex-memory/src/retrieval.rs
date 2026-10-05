use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::*;
use record::MemoryEmbedding;

impl MemoryStore {
    /// Configure semantic recall without making a model request.
    pub fn with_embedder(mut self, embedder: Arc<dyn ai_ex_core::EmbeddingPort>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    async fn request_embedding(&self, text: &str) -> Result<MemoryEmbedding, AppError> {
        let provider = self
            .embedder
            .as_ref()
            .ok_or_else(|| AppError::configuration("memory embedding is not configured"))?;
        let result = async {
            let embedding = MemoryEmbedding {
                space: provider.space().to_owned(),
                values: provider.embed(text).await?,
            };
            embedding.normalized()?;
            Ok(embedding)
        }
        .await;
        *self.inner.embedding_failed.write().await = result.is_err();
        result
    }

    pub(super) async fn enrich(&self, record: &mut MemoryRecord) -> Result<(), AppError> {
        // A failed provider must never leave a vector for the previous text attached.
        record.embedding = if self.embedder.is_some() {
            match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                self.request_embedding(&record::searchable_text(record)),
            )
            .await
            {
                Ok(result) => result.ok(),
                Err(_) => {
                    *self.inner.embedding_failed.write().await = true;
                    None
                }
            }
        } else {
            None
        };
        record.rebuild()
    }

    /// Explicit, all-or-nothing reindex of the active character. Never runs at startup.
    pub async fn reindex_embeddings(&mut self) -> Result<usize, AppError> {
        if !self.inner.enabled || self.embedder.is_none() {
            return Err(AppError::configuration(
                "reindex requires enabled memory and an embedding provider",
            ));
        }
        let profile_id = self.inner.profile_id.read().await;
        let _write_guard = self.inner.write_lock.lock().await;
        self.ensure_writer().await?;
        let mut records = self.inner.records.read().await.clone();
        let mut count = 0;
        for record in records
            .iter_mut()
            .filter(|record| record.profile_id == *profile_id)
        {
            record.embedding = Some(
                self.request_embedding(&record::searchable_text(record))
                    .await?,
            );
            record.rebuild()?;
            count += 1;
        }
        if count > 0 {
            self.replace_records(records).await?;
        }
        Ok(count)
    }

    pub(super) async fn recall_filtered(
        &self,
        kinds: Option<&[MemoryKind]>,
        query: &str,
        limit: usize,
        include_confirmed: bool,
    ) -> Result<Vec<Message>, AppError> {
        if !self.inner.enabled || limit == 0 {
            return Ok(Vec::new());
        }
        let profile_id = self.inner.profile_id.read().await;
        let query_terms = record::terms(query);
        if query_terms.is_empty() && !include_confirmed {
            return Ok(Vec::new());
        }
        let query_embedding = if self.embedder.is_some() && !query_terms.is_empty() {
            self.request_embedding(query).await.ok()
        } else {
            None
        };
        let normalized = query_embedding
            .as_ref()
            .map(MemoryEmbedding::normalized)
            .transpose()?
            .unwrap_or_default();
        let records = self.inner.records.read().await;
        let limit = limit.min(records.len());
        let mut best = BinaryHeap::with_capacity(limit.saturating_add(1));
        for (index, record) in records.iter().enumerate() {
            if record.profile_id != *profile_id
                || kinds.is_some_and(|kinds| !kinds.contains(&record.kind))
            {
                continue;
            }
            let confirmed = include_confirmed && record.source != MemorySource::Automatic;
            let overlap = query_terms.intersection(&record.terms).count();
            let cosine = query_embedding.as_ref().and_then(|query_vector| {
                let stored = record.embedding.as_ref()?;
                (stored.space == query_vector.space && normalized.len() == record.normalized.len())
                    .then(|| {
                        normalized
                            .iter()
                            .zip(&record.normalized)
                            .map(|(a, b)| f64::from(*a) * f64::from(*b))
                            .sum::<f64>()
                            .clamp(-1.0, 1.0)
                    })
            });
            if !confirmed && overlap == 0 && cosine.is_none_or(|score| score < 0.45) {
                continue;
            }
            let score = if confirmed {
                0
            } else if query_embedding.is_some() {
                let lexical = overlap as f64 / query_terms.len().max(1) as f64;
                ((0.2 * lexical + 0.8 * cosine.filter(|score| *score >= 0.45).unwrap_or(0.0))
                    * 1_000_000.0) as u64
            } else {
                overlap as u64
            };
            best.push(Reverse((
                confirmed,
                score,
                record.updated_ms.unwrap_or(record.created_ms),
                index,
            )));
            if best.len() > limit {
                best.pop();
            }
        }
        let mut ranked = best
            .into_iter()
            .map(|Reverse(rank)| rank)
            .collect::<Vec<_>>();
        ranked.sort_unstable_by(|a, b| b.cmp(a));
        Ok(ranked
            .into_iter()
            .map(|(_, _, _, index)| {
                let record = &records[index];
                Message::new(
                    Role::System,
                    format!(
                        "Relevant context memory [{}; source={:?}] — User: {} Assistant: {}",
                        record.kind.as_str(),
                        record.source,
                        record.user_text,
                        record.assistant_text
                    ),
                )
            })
            .collect())
    }
}
