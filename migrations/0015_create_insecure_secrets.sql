-- The account's secrets (`domains/secrets`: master key, session token),
-- kept here in plaintext only on a device with no usable OS credential store
-- and only once the user explicitly accepted it at registration
-- (`users::Service::create`'s `allow_insecure_storage`). Read after the OS
-- credential store, never before; wiped with every other table on logout.
CREATE TABLE IF NOT EXISTS insecure_secrets (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
