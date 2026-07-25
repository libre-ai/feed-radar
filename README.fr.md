[English](README.md) · **Français**

> [!NOTE]
> **Réservé · futur foyer de Radar** — reconstruit dans le dépôt de base canonique [`libre-ai/libre-ai`](https://github.com/libre-ai/libre-ai) ([topologie multi-dépôts, ADR-0008](https://github.com/libre-ai/libre-ai/blob/main/docs/adr/0008-multi-repo-target-topology-and-brand.md)).
> Ce dépôt rouvrira comme dépôt produit réel lorsque le propriétaire l'activera, consommant la base comme dépendance versionnée. Les fondations décrites ci-dessous sont **en cours de construction** — avec des liens vers le code qui existe déjà.

# Radar

**Sélection explicable de flux et curation portable.** Abonnez-vous à des flux (RSS, Atom, JSON Feed), appliquez des règles déterministes visibles pour décider quels articles conserver, inspectez pourquoi chaque décision a été prise règle par règle, et exportez un ensemble curé que vous contrôlez. Les travailleurs cherchent des sources non fiables ; aucun flux ne devient fiable simplement en étant ingéré.

Le cas canonique auquel il répond : _« Aidez-moi à lire uniquement ce que j'ai choisi, expliquez pourquoi »_ — exposant **chaque** règle qui a correspondu ou échoué, sans **jamais** de classement opaque ou de surprise algorithmique. Conçu pour être local-d'abord, pour la portabilité OPML/RSS/Atom/JSON Feed, et pour la relecture : comparez comment les mêmes articles auraient été décidés sous une autre version de règles sans réécrire l'historique.

## Ce qui le distingue

- **Décisions explicables, pas de scores.** Chaque article conservé ou rejeté est tracé jusqu'aux règles spécifiques qui ont correspondu ou échoué — vous voyez le raisonnement, règle par règle, chaque fois.
- **Curation portable.** Les règles, les abonnements et les décisions curées s'exportent avec provenance au format OPML/JSON standard ; vous possédez les données que vous avez écrites.
- **Relecture sans réécriture.** Comparez comment une autre version de règles aurait décidé les mêmes articles historiques sans mutiler les décisions originales. L'historique est d'ajout uniquement.
- **Sûr avec des sources hostiles.** L'analyse des flux est confinée dans un moteur déterministe sans capacité ; les articles sont normalisés et dédupliqués avant d'atteindre l'interface utilisateur. Les flux restent non fiables jusqu'à ce que _vous_ décidiez qu'ils comptent.
- **Refus fermé sur les mauvaises sources.** La politique de destination pré-réseau (porte SSRF) refuse les plages loopback, privées, de métadonnées et les tours DNS-rebinding avant qu'une seule socket ne s'ouvre.

## État — spécifié publiquement, fondations en construction

Radar est reconstruit à partir de contrats verrouillés. Il **n'est pas encore publié** ; l'analyseur de flux déterministe et l'évaluateur de règles viennent ensuite, et la couche contrat est déjà éprouvée :

| Fondation                                                                   | État          | Preuve                                                                                                                                                               |
| --------------------------------------------------------------------------- | ------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Schémas contrats v1/v2** — formats verrouillés pour flux, règles, exports | ✅ construits | 8 schémas JSON + monde WIT, PROFILE.md normatif 275 lignes ([#40](https://github.com/libre-ai/libre-ai/pull/40)–[#46](https://github.com/libre-ai/libre-ai/pull/46)) |
| **Corpus de vecteurs golden** — 43 cas d'analyse, limites de sécurité       | ✅ publié     | `contracts/fixtures/radar-engine-v2/{golden-vectors.v1.json, security-vectors.v1.json, adversarial/, positive/}`                                                     |
| **Porte SSRF pré-réseau** — politique de destination refus-fermé            | ✅ construit  | `src/security/destination-policy.ts` + tests, classification IPv4/IPv6, défaite DNS-rebinding ([#161](https://github.com/libre-ai/libre-ai/pull/161))                |
| **OpenAPI v2** — surface serveur et travailleur                             | ✅ spécifié   | `contracts/openapi/radar.v2.yaml` + protocole de domaine dans le cahier des charges produit                                                                          |
| Analyseur de flux hostile — composant Rust/WASM                             | ⏳ suite      | Interface WIT verrouillée, conteneur prêt, en attente d'implémentation                                                                                               |
| Évaluateur de règles déterministe — logique de décision de politique        | ⏳ suite      | Spécification en PROFILE.md ; vecteurs golden ciblant sémantique exacte                                                                                              |
| Quarantaine réseau / modèle de bail travailleur — récupération bornée       | ⏳ suite      | Architecture esquissée ; idempotence, déduplication et isolation des credentials à définir                                                                           |
| API locataire + cockpit UI — abonnements, règles, décisions, export         | ⏳ vague 3–4  | Différé : en attente de fondation analyseur + moteur de règles                                                                                                       |

Ce dépôt est une réserve publique ; l'implémentation legacy qu'il porte encore est gelée pour référence, et la reconstruction se passe dans le dépôt de base jusqu'à l'activation (vague 4). **Cible de référence :** Inoreader ([inoreader.com](https://www.inoreader.com)) — atteinte par une curation déterministe et auditable plutôt que par la découverte algorithmique.

## Comment ça fonctionne

1. **S'abonner** — un utilisateur soumet une URL de flux HTTP(S) ; la porte pré-réseau classe la destination avant toute récupération ; un aperçu scanne l'en-tête du flux.
2. **Récupérer et normaliser** — les travailleurs récupèrent les flux dans un environnement de quarantaine et les analysent en un schéma normalisé canonique (Atom, RSS ou JSON Feed deviennent tous le même contrat).
3. **Évaluer les règles** — un moteur déterministe sans capacité applique une version de règles versionnée à chaque article et enregistre la décision et les règles correspondantes.
4. **Inspecter et rejouer** — les utilisateurs voient la source, les champs normalisés, les règles appliquées et le résultat décidé. Ils peuvent rejouer avec une autre version de règles pour comparer les décisions sans mutiler l'historique.
5. **Exporter et supprimer** — les articles curés, les règles et les abonnements s'exportent en JSON avec provenance ; les utilisateurs peuvent supprimer des sources et leurs données conservées à la demande.

## Architecture — assemblée à partir de briques interopérables

Radar est un produit assemblé à partir de briques versionnées indépendamment ; chacune est utilisable et testable seule, et le produit est leur composition (la cible multi-dépôts de [l'ADR-0008](https://github.com/libre-ai/libre-ai/blob/main/docs/adr/0008-multi-repo-target-topology-and-brand.md)).

| Brique                                          | Rôle                                                     | Interface exposée / consommée                                                                                                                                                  |
| ----------------------------------------------- | -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **`radar-engine`** (Rust → composant WASM)      | Le moteur d'analyse de flux déterministe et d'évaluation | Monde WIT `radar-engine-v2` : `parse(flux-octets, limites) → éléments`, `evaluate(éléments, jeu-de-règles) → décisions`                                                        |
| **`@libre-ai/destination-policy`** (TypeScript) | Porte pré-réseau refus-fermé                             | `isForbiddenDestination(ip)`, `evaluateFetchDestination(url)`, `checkRedirect(...)` → enum refus ou ✓                                                                          |
| **`@libre-ai/web-platform`**                    | Fondation SSR / BFF Bun                                  | Gestionnaire de requêtes, session authentifiée, contexte locataire                                                                                                             |
| **Contrats**                                    | Surface d'interopérabilité verrouillée                   | `feed-fetch.v1`, `curation-rule-set.v2`, `radar-normalized-{feed,item}.v1`, `radar-rule-evaluation.v1`, `curated-item-export.v2`, `radar.v2.yaml`, monde WIT + vecteurs golden |

Le travailleur reçoit un jeton d'autorisation atténué limité à un locataire et une source de flux unique ; le moteur ne détient aucun jeton, n'ouvre aucun réseau et ne reçoit aucun octet de réponse brut — il ne fonctionne que sur des entrées normalisées sous des limites octet/élément/profondeur déclarées.

## Où se déroule le travail

Tout le développement actif est dans le dépôt de base, sous :

- `apps/radar` — l'hôte produit (API, orchestration des travailleurs, cockpit UI)
- `contracts/schemas/` — récupération de flux, règles de curation, articles et exports normalisés (v1/v2)
- `contracts/wit/radar-engine-v2/` — la définition du monde WIT et le PROFILE.md normatif
- `contracts/fixtures/radar-engine-v2/` — vecteurs golden et corpus de sécurité
- `contracts/openapi/radar.v2.yaml` — surface d'API et points d'extrémité travailleur
- [`docs/apps/radar.md`](https://github.com/libre-ai/libre-ai/blob/main/docs/apps/radar.md) — le cahier des charges produit complet

Pour suivre l'avancement ou contribuer, ouvrez issues et pull requests dans [`libre-ai/libre-ai`](https://github.com/libre-ai/libre-ai). Ce dépôt reste réservé jusqu'à son activation.

## Licence

Multi-licence, déclarée par chemin avec [REUSE](https://reuse.software/) dans [`REUSE.toml`](REUSE.toml) :

- `EUPL-1.2` — l'espace de travail Rust applicatif, la surface UI, les migrations PostgreSQL, le manifeste de sécurité de base de données, les scripts, la suite end-to-end et les workflows sous `.github/` ;
- `Apache-2.0` — les schémas d'interopérabilité sous [`schemas/`](schemas) et le corpus d'exemples sous [`examples/`](examples) ;
- `CC-BY-4.0` — la documentation éditoriale et les registres de projet.

[`LICENSE`](LICENSE) en est le résumé lisible ; la politique canonique — portée, précédence, concessions historiques, données et contributions — est la [politique de licences de l'organisation](https://github.com/libre-ai/libre-ai/blob/main/LICENSING.md). Les révisions publiées antérieurement sous MIT restent disponibles sous ces termes ; aucune concession de licence antérieure n'est retirée.
