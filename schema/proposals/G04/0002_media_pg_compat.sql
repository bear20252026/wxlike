-- M012（草案，已登记为草案编号，未接入 CI）— media 兼容视图（PostgreSQL）
--
-- 目的：旧平台 SQL 继续读写 `media`，底层落到 G04 的 `media_object` /
-- `media_ref`。这是 M011 之后的第二块兼容层。
--
-- ⚠️ 已知硬约束（必须在代码层拍板后才能把本文件转为正式迁移）：
-- `domain::media::store` 的 `INSERT INTO media ... ON CONFLICT(id) DO UPDATE`
-- 明确不能作用在 PostgreSQL 视图上（本机 PG16 实测报错：
-- "there is no unique or exclusion constraint matching the ON CONFLICT
-- specification"）。本文件先提供**读兼容 + 普通 INSERT/UPDATE/DELETE 触发器**；
-- `media_insert_trg` 内部已经对 `media_object` 做 `ON CONFLICT` upsert，
-- 所以只要调用点在 PG profile 下去掉 `ON CONFLICT` 即可跑通。采用方案：
--   (a) `store_media` 按 `Db::dialect()` 分支：SQLite 保留原 upsert；
--       PostgreSQL 走普通 INSERT，由下面的 media_insert_trg 承接。
--       这是一条 SQL 分支，不改任何 repo 签名；等 PG 连接实现后再评估是否
--       把 SQLite 分支也统一掉。
--   (b) 不建视图，改同名兼容表；会出现双源，和“单一化定义”冲突，不采用。
--
-- 幂等：CREATE TABLE IF NOT EXISTS / CREATE OR REPLACE VIEW/FUNCTION /
-- DROP TRIGGER IF EXISTS。
-- 回滚：先删 media 视图/触发器/函数，再删 media_ref/media_object；不改旧表。

BEGIN;

-- ---------------------------------------------------------------------------
-- 1. 规范对象表（G04 的 PostgreSQL 形态）
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS media_object (
    object_id    TEXT PRIMARY KEY,
    owner        TEXT NOT NULL,
    size         BIGINT NOT NULL CHECK (size >= 0),
    digest       TEXT NOT NULL,
    mime         TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('ready', 'deleted')),
    access_epoch BIGINT NOT NULL DEFAULT 0,
    created_at   BIGINT NOT NULL,
    deleted_at   BIGINT
);
CREATE INDEX IF NOT EXISTS idx_media_object_owner
    ON media_object(owner, created_at DESC);

CREATE TABLE IF NOT EXISTS media_ref (
    ref_id     TEXT PRIMARY KEY,
    object_id  TEXT NOT NULL REFERENCES media_object(object_id),
    ref_type   TEXT NOT NULL,
    ref_id_ext TEXT NOT NULL,
    owner      TEXT NOT NULL,
    audience   TEXT NOT NULL,
    purpose    TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    deleted_at BIGINT
);
CREATE INDEX IF NOT EXISTS idx_media_ref_object
    ON media_ref(object_id, deleted_at);
CREATE UNIQUE INDEX IF NOT EXISTS uq_media_ref_logical
    ON media_ref(ref_type, ref_id_ext, object_id, audience)
    WHERE deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- 2. 兼容视图：media
-- ---------------------------------------------------------------------------
-- 旧列 -> 新列映射：
--   id           -> media_object.object_id
--   size         -> media_object.size
--   content_type -> media_object.mime
--   uploader     -> media_object.owner
--   conv_id      -> media_ref.ref_id_ext（ref_type='conv'）
--   public       -> media_ref.audience='public' 是否存在
--   has_thumb    -> 兼容期固定 0；缩略图信息在 media_job outputs，不在这张视图
--   created_at   -> media_object.created_at
--
-- 只支持"一个对象一个 conv ref"的旧读法：同一对象挂多个会话时，取最早
-- 创建的非删除 ref；旧 `media.conv_id` 本身也只存一个会话。

CREATE OR REPLACE VIEW media AS
SELECT
    o.object_id AS id,
    o.size,
    o.mime AS content_type,
    o.owner AS uploader,
    COALESCE((
        SELECT r.ref_id_ext
        FROM media_ref r
        WHERE r.object_id = o.object_id
          AND r.ref_type = 'conv'
          AND r.deleted_at IS NULL
        ORDER BY r.created_at, r.ref_id
        LIMIT 1
    ), '') AS conv_id,
    CASE WHEN EXISTS (
        SELECT 1 FROM media_ref r
        WHERE r.object_id = o.object_id
          AND r.audience = 'public'
          AND r.deleted_at IS NULL
    ) THEN 1 ELSE 0 END AS public,
    0::integer AS has_thumb,
    o.created_at
FROM media_object o;

-- ---------------------------------------------------------------------------
-- 3. 写兼容（普通 INSERT/UPDATE/DELETE；旧 upsert 在调用点改为普通 INSERT）
-- ---------------------------------------------------------------------------
-- 视图没有唯一约束，PostgreSQL 不允许 `INSERT ... ON CONFLICT` 直接作用在
-- 视图上。兼容做法：调用点去掉 `ON CONFLICT`，由本触发器在 `media_object`
-- 上做幂等 upsert；`conv_id` / `public` 的“保留第一次归属、公开不回收”语义
-- 由下面的 ref 写入顺序承接（读视图取最早的非删除 ref）。

CREATE OR REPLACE FUNCTION media_insert_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO media_object (
        object_id, owner, size, digest, mime, status, access_epoch, created_at
    ) VALUES (
        NEW.id, NEW.uploader, NEW.size, NEW.id, NEW.content_type,
        'ready', 0, NEW.created_at
    )
    ON CONFLICT (object_id) DO UPDATE SET
        -- 与旧 SQL 的 CASE 一致：归属只保留第一次，不因后一次上传改写。
        size = EXCLUDED.size,
        mime = EXCLUDED.mime;

    IF COALESCE(NEW.conv_id, '') <> '' THEN
        INSERT INTO media_ref (
            ref_id, object_id, ref_type, ref_id_ext, owner, audience, purpose, created_at
        ) VALUES (
            'media:' || NEW.id || ':conv:' || NEW.conv_id,
            NEW.id, 'conv', NEW.conv_id, NEW.uploader, 'conversation', 'attachment', NEW.created_at
        )
        ON CONFLICT DO NOTHING;
    END IF;

    IF COALESCE(NEW.public, 0) <> 0 THEN
        INSERT INTO media_ref (
            ref_id, object_id, ref_type, ref_id_ext, owner, audience, purpose, created_at
        ) VALUES (
            'media:' || NEW.id || ':public',
            NEW.id, 'public', '', NEW.uploader, 'public', 'public', NEW.created_at
        )
        ON CONFLICT DO NOTHING;
    END IF;

    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS media_insert_trg ON media;
CREATE TRIGGER media_insert_trg
    INSTEAD OF INSERT ON media
    FOR EACH ROW EXECUTE FUNCTION media_insert_fn();

CREATE OR REPLACE FUNCTION media_update_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    UPDATE media_object SET
        owner = NEW.uploader,
        size = NEW.size,
        mime = NEW.content_type
    WHERE object_id = OLD.id;

    -- conv_id 是单值旧列：先清旧 conv ref，再按新值重建。
    DELETE FROM media_ref
    WHERE object_id = OLD.id AND ref_type = 'conv';

    IF COALESCE(NEW.conv_id, '') <> '' THEN
        INSERT INTO media_ref (
            ref_id, object_id, ref_type, ref_id_ext, owner, audience, purpose, created_at
        ) VALUES (
            'media:' || NEW.id || ':conv:' || NEW.conv_id,
            NEW.id, 'conv', NEW.conv_id, NEW.uploader, 'conversation', 'attachment', NEW.created_at
        )
        ON CONFLICT DO NOTHING;
    END IF;

    DELETE FROM media_ref
    WHERE object_id = OLD.id AND ref_type = 'public';

    IF COALESCE(NEW.public, 0) <> 0 THEN
        INSERT INTO media_ref (
            ref_id, object_id, ref_type, ref_id_ext, owner, audience, purpose, created_at
        ) VALUES (
            'media:' || NEW.id || ':public',
            NEW.id, 'public', '', NEW.uploader, 'public', 'public', NEW.created_at
        )
        ON CONFLICT DO NOTHING;
    END IF;

    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS media_update_trg ON media;
CREATE TRIGGER media_update_trg
    INSTEAD OF UPDATE ON media
    FOR EACH ROW EXECUTE FUNCTION media_update_fn();

CREATE OR REPLACE FUNCTION media_delete_fn()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM media_ref WHERE object_id = OLD.id;
    DELETE FROM media_object WHERE object_id = OLD.id;
    RETURN OLD;
END;
$$;

DROP TRIGGER IF EXISTS media_delete_trg ON media;
CREATE TRIGGER media_delete_trg
    INSTEAD OF DELETE ON media
    FOR EACH ROW EXECUTE FUNCTION media_delete_fn();

COMMIT;

-- 回滚（手工）：
--   DROP TRIGGER IF EXISTS media_insert_trg ON media;
--   DROP TRIGGER IF EXISTS media_update_trg ON media;
--   DROP TRIGGER IF EXISTS media_delete_trg ON media;
--   DROP VIEW IF EXISTS media;
--   DROP FUNCTION IF EXISTS media_insert_fn();
--   DROP FUNCTION IF EXISTS media_update_fn();
--   DROP FUNCTION IF EXISTS media_delete_fn();
--   DROP TABLE IF EXISTS media_ref;
--   DROP TABLE IF EXISTS media_object;
