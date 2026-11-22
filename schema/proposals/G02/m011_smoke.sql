BEGIN;
INSERT INTO users(id, name, pw_salt, pw_hash, pw_rounds, pw_algo, created_at)
VALUES ('u1', 'alice', '', 'phc', 0, 'argon2id', 1);

SELECT id, name, deleted_at FROM users WHERE id = 'u1';

UPDATE users SET name = 'alice2', deleted_at = 99 WHERE id = 'u1';
SELECT id, name, deleted_at FROM users WHERE id = 'u1';

INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, device_name, ip, last_seen)
VALUES ('t1', 'u1', 1, 100, 's1', 'd1', 'phone', '127.0.0.1', 1);

SELECT token, rowid, audience FROM sessions WHERE token = 't1';
UPDATE sessions SET last_seen = 50 WHERE token = 't1';
SELECT token, last_seen FROM sessions WHERE token = 't1';

DELETE FROM sessions WHERE rowid IN (SELECT rowid FROM sessions WHERE expires_at <= 100 LIMIT 1);
SELECT count(*) AS sessions_after_delete FROM sessions;

DELETE FROM users WHERE id = 'u1';
SELECT count(*) AS identity_user_after_delete FROM identity_user;
SELECT count(*) AS sessions_after_user_delete FROM identity_session_token;
COMMIT;
