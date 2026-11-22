BEGIN;
INSERT INTO media(id, size, content_type, uploader, conv_id, public, has_thumb, created_at)
VALUES ('m1', 12, 'image/png', 'u1', 'c1', 0, 0, 10);

SELECT id, size, content_type, uploader, conv_id, public, has_thumb, created_at
FROM media WHERE id = 'm1';

INSERT INTO media(id, size, content_type, uploader, conv_id, public, has_thumb, created_at)
VALUES ('m1', 13, 'image/jpeg', 'u1', 'c1', 0, 0, 10);

SELECT id, size, content_type FROM media WHERE id = 'm1';

UPDATE media SET public = 1, conv_id = 'c2' WHERE id = 'm1';
SELECT id, conv_id, public FROM media WHERE id = 'm1';

DELETE FROM media WHERE id = 'm1';
SELECT count(*) AS media_after_delete FROM media;
SELECT count(*) AS object_after_delete FROM media_object;
COMMIT;
