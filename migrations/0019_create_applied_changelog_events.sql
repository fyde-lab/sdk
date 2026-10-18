-- SHA-256 digests of the encrypted content of every changelog event this
-- database applied (`changelog::Service::consume`), so an event the server
-- serves again under a new id (a replay: it can't forge or re-encrypt one,
-- but it can repeat an old one) is recognised and skipped instead of
-- rolling an installed script or a document's metadata back. Wiped with
-- every other table on logout, along with the cursor.
CREATE TABLE IF NOT EXISTS applied_changelog_events (
    digest BLOB PRIMARY KEY NOT NULL
) WITHOUT ROWID;
