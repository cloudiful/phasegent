use crate::providers::api::ForgejoError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{
    RedmineNewRelation, RedmineRelationCollection, RedmineRelationResponse, RedmineRelationType,
    RelationSummary,
};

impl RedmineProvider {
    /// List the relations of a single issue
    /// (`/issues/:id/relations.json`). Each relation is rendered from the
    /// queried issue's viewpoint so inverse names appear correctly.
    pub fn list_relations(&self, issue: u64) -> Result<Vec<RelationSummary>, ForgejoError> {
        let path = format!("issues/{issue}/relations.json");
        let collection: RedmineRelationCollection = self.http.get(&path, &[], "relation list")?;
        Ok(collection
            .relations
            .into_iter()
            .map(|relation| relation.into_summary(issue))
            .collect())
    }

    /// Create a relation from `issue` to `to` with a canonical `--type`.
    /// `delay` is only meaningful for `precedes` and is omitted otherwise.
    /// Returns the created relation, matching the shared provider create
    /// shape.
    pub fn create_relation(
        &self,
        issue: u64,
        to: u64,
        relation_type: RedmineRelationType,
        delay: Option<u64>,
    ) -> Result<RelationSummary, ForgejoError> {
        let path = format!("issues/{issue}/relations.json");
        let payload = RedmineNewRelation::new(to, relation_type.as_str(), delay);
        let response: RedmineRelationResponse =
            self.http.post(&path, &payload, "relation create")?;
        Ok(response.relation.into_summary(issue))
    }

    /// Delete a relation by its numeric id (`DELETE /relations/:id.json`).
    /// Mirrors the shared provider shape: a successful delete returns no body.
    pub fn delete_relation(&self, relation_id: u64) -> Result<(), ForgejoError> {
        let path = format!("relations/{relation_id}.json");
        self.http
            .delete::<serde_json::Value>(&path, "relation delete")
            .map(|_| ())
    }
}
