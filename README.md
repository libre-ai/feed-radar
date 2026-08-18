# Radar

Intelligence de flux locale : OPML, RSS 2.0 et Atom 1.0 vers des exports curés explicables (couche 1).

Pour les personnes qui suivent beaucoup de sources d'information, qui rencontre des fils d'actualité ingérables et des algorithmes de recommandation opaques, ce projet permet de transformer ses abonnements en sélections lisibles dont chaque choix est justifié, en produisant des exports curés où chaque sélection est expliquée par une règle lisible, sans dépendre de : aucun algorithme opaque, aucune donnée hors de sa machine.

## État du projet

<!-- libre-ai:project-status:begin -->
<!-- Section générée depuis project.v1.yaml — ne pas éditer à la main. -->

- Situation actuelle : L'évaluateur de règles (vérifié byte-exact contre la référence policy-core) et la politique de destinations sont greffés et verts ; la surface produit complète (imports, interface, exports de bout en bout) reste à construire.
- Maturité : specified
- Exposition : spec-published
- Confiance : medium
- Preuves vérifiées le : 2026-08-18
- Avancement : 0 % du périmètre actuellement déclaré

<!-- libre-ai:project-status:end -->

## Vérifier

- `bun install && bun run check` — la chaîne de gates du dépôt, tests inclus.
- La fiche [`project.v1.yaml`](./project.v1.yaml) est l'autorité de l'état du projet ; la section « État du projet » ci-dessus en est générée et un gate de flotte échoue si elles divergent.
- La provenance de chaque chemin migré depuis le hub est tracée dans l'index de migration de `libre-ai/libre-ai` (`ecosystem/migration-index.v1.yaml`).
