-- Supporting index for keyset pagination on the article list endpoint.
--
-- `GET /api/v1/articles` orders by `published_at DESC NULLS LAST, id DESC` and
-- resumes a page with a predicate on that same pair. No pre-existing index
-- covers it: `idx_articles_user` stops at the tenant column, and
-- `idx_articles_unread` is partial (`is_read = FALSE AND is_hidden = FALSE`)
-- and carries no tiebreaker, so it cannot satisfy the ordering that makes the
-- cursor total.
--
-- Additive only. No historical migration is edited, no column is altered, and
-- the index is not unique, so nothing that was previously accepted becomes a
-- constraint violation.
CREATE INDEX idx_articles_user_published_id
    ON articles (user_id, published_at DESC NULLS LAST, id DESC);
