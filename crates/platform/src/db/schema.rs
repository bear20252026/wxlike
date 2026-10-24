//! 平台库全部建表 SQL（G00-02 自 db.rs 纯移动：表结构是数据不是逻辑，
//! 独立成模块让评审整块看 diff）。常量内容与拆分前逐字节一致。

pub(super) const SCHEMA: &str = r#"
            -- 幂等重放：agent 会重试，同一个 key 必须返回第一次的结果。
            -- 只存"写操作"的结果，读操作不需要。
            CREATE TABLE IF NOT EXISTS idempotency (
                key        TEXT PRIMARY KEY,
                response   TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS conv_owner (
                conv_id    TEXT PRIMARY KEY,
                owner_type TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS users (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL UNIQUE,
                pw_salt      TEXT NOT NULL,
                pw_hash      TEXT NOT NULL,
                pw_rounds    INTEGER NOT NULL,
                pw_algo      TEXT NOT NULL DEFAULT 'pbkdf2',
                totp_secret  TEXT,
                totp_enabled INTEGER NOT NULL DEFAULT 0,
                avatar       TEXT NOT NULL DEFAULT '',
                signature    TEXT NOT NULL DEFAULT '',
                created_at   INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                token       TEXT PRIMARY KEY,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);

            CREATE TABLE IF NOT EXISTS friend_requests (
                id          TEXT PRIMARY KEY,
                from_uid    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                to_uid      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                message     TEXT NOT NULL DEFAULT '',
                status      TEXT NOT NULL DEFAULT 'pending',
                created_at  INTEGER NOT NULL,
                UNIQUE(from_uid, to_uid)
            );
            CREATE INDEX IF NOT EXISTS idx_fr_to ON friend_requests(to_uid, status);
            CREATE INDEX IF NOT EXISTS idx_fr_from ON friend_requests(from_uid, status);

            CREATE TABLE IF NOT EXISTS friends (
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                friend_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                remark      TEXT NOT NULL DEFAULT '',
                source      TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL,
                PRIMARY KEY (user_id, friend_id)
            );

            -- Append-only per-user contact log: the cursor source for
            -- /api/friends/changes (address-book sync).
            CREATE TABLE IF NOT EXISTS contact_log (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                subject_id  TEXT NOT NULL,
                kind        TEXT NOT NULL,
                changed_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_cl_user ON contact_log(user_id, seq);

            -- P3 ADR-24 拉黑：前置闸门（申请/陌生人请求/hub 收件过滤共用）
            CREATE TABLE IF NOT EXISTS blocks (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                blocked_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, blocked_id)
            );

            -- P3 ADR-24 陌生人消息请求箱（不变式 9）：非好友首条消息不产生会话
            CREATE TABLE IF NOT EXISTS message_requests (
                id         TEXT PRIMARY KEY,
                from_uid   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                to_uid     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text       TEXT NOT NULL DEFAULT '',
                status     TEXT NOT NULL DEFAULT 'pending',
                created_at INTEGER NOT NULL,
                UNIQUE (from_uid, to_uid, status)
            );
            CREATE INDEX IF NOT EXISTS idx_mr_to ON message_requests(to_uid, status);

            -- P3 用户设置：是否允许陌生人发起（默认允许；流程必须存在）
            CREATE TABLE IF NOT EXISTS user_settings (
                user_id         TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                allow_strangers INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS groups (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                owner_uid   TEXT NOT NULL REFERENCES users(id),
                created_at  INTEGER NOT NULL
            );
            -- P3 群管理补齐列（老库原地补列见 migrate_groups_admin）
            -- announcement/approval/all_muted

            -- P3 群公告（单条置顶）
            CREATE TABLE IF NOT EXISTS group_announcement (
                group_id  TEXT PRIMARY KEY REFERENCES groups(id) ON DELETE CASCADE,
                text      TEXT NOT NULL DEFAULT '',
                media_id  TEXT NOT NULL DEFAULT '',
                pinned_by TEXT NOT NULL DEFAULT '',
                updated_at INTEGER NOT NULL
            );
            -- P3 单人禁言（until_ms 定时自然解除）
            CREATE TABLE IF NOT EXISTS group_mutes (
                group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                until_ms INTEGER NOT NULL,
                PRIMARY KEY (group_id, user_id)
            );
            -- P3 邀请 token（带过期；审批开的群入群转请求）
            CREATE TABLE IF NOT EXISTS group_invites (
                token      TEXT PRIMARY KEY,
                group_id   TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                created_by TEXT NOT NULL REFERENCES users(id),
                expires_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_gi_group ON group_invites(group_id);
            -- P3 入群审批请求
            CREATE TABLE IF NOT EXISTS group_join_requests (
                group_id     TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                status       TEXT NOT NULL DEFAULT 'pending',
                requested_at INTEGER NOT NULL,
                PRIMARY KEY (group_id, user_id)
            );

            -- P3 支柱 C：社区/频道/话题（schema 形状借用 TangSengDaoDaoServer
            -- 的群/成员表：角色 0 成员 1 群主 2 管理员；tenant_id 预留列）
            CREATE TABLE IF NOT EXISTS communities (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL,
                owner_uid    TEXT NOT NULL REFERENCES users(id),
                member_limit INTEGER NOT NULL DEFAULT 10000,
                announcement TEXT NOT NULL DEFAULT '',
                tenant_id    TEXT NOT NULL DEFAULT 'default',
                created_at   INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS community_members (
                community_id TEXT NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role         INTEGER NOT NULL DEFAULT 0,
                joined_at    INTEGER NOT NULL,
                PRIMARY KEY (community_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS channels (
                id           TEXT PRIMARY KEY,
                community_id TEXT NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
                name         TEXT NOT NULL,
                kind         TEXT NOT NULL DEFAULT 'text',
                tenant_id    TEXT NOT NULL DEFAULT 'default',
                created_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_community ON channels(community_id);
            CREATE TABLE IF NOT EXISTS threads (
                id         TEXT PRIMARY KEY,
                channel_id TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                root_seq   INTEGER NOT NULL,
                title      TEXT NOT NULL DEFAULT '',
                created_by TEXT NOT NULL REFERENCES users(id),
                tenant_id  TEXT NOT NULL DEFAULT 'default',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_threads_channel ON threads(channel_id);
            CREATE TABLE IF NOT EXISTS thread_messages (
                thread_id  TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                sender     TEXT NOT NULL,
                text       TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS thread_subs (
                thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                PRIMARY KEY (thread_id, user_id)
            );

            -- P3 多级免打扰（ADR-19 决策层）：每用户 × 每会话
            CREATE TABLE IF NOT EXISTS notification_settings (
                user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                conv_id  TEXT NOT NULL,
                level    TEXT NOT NULL DEFAULT 'all',
                keywords TEXT NOT NULL DEFAULT '[]',
                PRIMARY KEY (user_id, conv_id)
            );

            -- P3 审计日志（不变式 7：与运行日志——tracing/Loki——物理分离，
            -- 这里是独立审计表；检索面随 P6 adminx 落地）
            CREATE TABLE IF NOT EXISTS audit_log (
                id     INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms  INTEGER NOT NULL,
                actor  TEXT NOT NULL DEFAULT '',
                action TEXT NOT NULL,
                target TEXT NOT NULL DEFAULT '',
                detail TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_audit_actor ON audit_log(actor, at_ms);
            CREATE TABLE IF NOT EXISTS group_members (
                group_id    TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                role        TEXT NOT NULL DEFAULT 'member',
                my_remark   TEXT NOT NULL DEFAULT '',
                joined_at   INTEGER NOT NULL,
                PRIMARY KEY (group_id, user_id)
            );
            CREATE INDEX IF NOT EXISTS idx_gm_user ON group_members(user_id);

            CREATE TABLE IF NOT EXISTS media (
                id            TEXT PRIMARY KEY,
                size          INTEGER NOT NULL,
                content_type  TEXT NOT NULL DEFAULT 'application/octet-stream',
                uploader      TEXT NOT NULL DEFAULT '',
                -- 上传时声明所属会话：只有会话成员能下载。
                -- 空串 = 未挂到会话（只有上传者能取），不是"公开"。
                conv_id       TEXT NOT NULL DEFAULT '',
                -- 头像这类"所有登录用户都要能看"的资源。聊天附件一律 0。
                public        INTEGER NOT NULL DEFAULT 0,
                has_thumb     INTEGER NOT NULL DEFAULT 0,
                created_at    INTEGER NOT NULL
            );

            -- WP-09: a virtual ledger only. Amounts are integer minor units
            -- (分), never floats, so conservation is exact.
            CREATE TABLE IF NOT EXISTS wallets (
                user_id   TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                balance   INTEGER NOT NULL DEFAULT 0 CHECK (balance >= 0)
            );
            CREATE TABLE IF NOT EXISTS ledger (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind         TEXT NOT NULL,
                amount       INTEGER NOT NULL,
                counterparty TEXT NOT NULL DEFAULT '',
                ref_id       TEXT NOT NULL DEFAULT '',
                memo         TEXT NOT NULL DEFAULT '',
                created_at   INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_ledger_user ON ledger(user_id, id);
            CREATE TABLE IF NOT EXISTS redpackets (
                id            TEXT PRIMARY KEY,
                from_uid      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                conv_id       TEXT NOT NULL DEFAULT '',
                total         INTEGER NOT NULL,
                count         INTEGER NOT NULL,
                claimed       INTEGER NOT NULL DEFAULT 0,
                claimed_total INTEGER NOT NULL DEFAULT 0,
                claimers      TEXT NOT NULL DEFAULT '[]',
                memo          TEXT NOT NULL DEFAULT '',
                status        TEXT NOT NULL DEFAULT 'open',
                created_at    INTEGER NOT NULL,
                expire_at     INTEGER NOT NULL
            );

            -- WP-07 moments. `seq` is the paging cursor (monotonic, so equal
            -- timestamps cannot create duplicates or gaps); `id` is the public id.
            CREATE TABLE IF NOT EXISTS moments (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                id          TEXT NOT NULL UNIQUE,
                author      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text        TEXT NOT NULL DEFAULT '',
                media       TEXT NOT NULL DEFAULT '[]',
                visibility  TEXT NOT NULL DEFAULT 'friends',
                deleted     INTEGER NOT NULL DEFAULT 0,
                created_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_moments_author ON moments(author, seq);
            CREATE TABLE IF NOT EXISTS moment_likes (
                moment_id   TEXT NOT NULL REFERENCES moments(id) ON DELETE CASCADE,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at  INTEGER NOT NULL,
                PRIMARY KEY (moment_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS moment_comments (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                id          TEXT NOT NULL UNIQUE,
                moment_id   TEXT NOT NULL REFERENCES moments(id) ON DELETE CASCADE,
                author      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text        TEXT NOT NULL,
                reply_to    TEXT NOT NULL DEFAULT '',
                deleted     INTEGER NOT NULL DEFAULT 0,
                created_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mc_moment ON moment_comments(moment_id, seq);

            -- WP-12: fetchable E2EE pre-key bundles. `otks` is a JSON array of
            -- unclaimed one-time keys — [key, signature] pairs since AR-1 —
            -- claiming pops from it (see keys.rs). `signature` is the owner's
            -- Ed25519 signature over (user_id, identity); legacy rows default
            -- to '' and are rejected by clients (fail closed).
            CREATE TABLE IF NOT EXISTS prekey_bundles (
                user_id          TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                identity         TEXT NOT NULL,
                identity_signing TEXT NOT NULL DEFAULT '',
                signature        TEXT NOT NULL DEFAULT '',
                otks             TEXT NOT NULL DEFAULT '[]',
                updated_at       INTEGER NOT NULL
            );

            -- WP-06: push targets. One row per (user, platform): a re-register
            -- on the same platform replaces the token ("keep the latest"),
            -- different platforms coexist.
            CREATE TABLE IF NOT EXISTS devices (
                user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                platform      TEXT NOT NULL,
                device_token  TEXT NOT NULL,
                last_seen     INTEGER NOT NULL,
                PRIMARY KEY (user_id, platform)
            );

            -- P1 favorites: a snapshot per favorited message (survives the
            -- original being recalled or deleted). Structured columns, not a
            -- JSON blob, so paging stays an index walk.
            CREATE TABLE IF NOT EXISTS favorites (
                id            TEXT PRIMARY KEY,
                user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                source_conv   TEXT NOT NULL DEFAULT '',
                source_msg_id TEXT NOT NULL DEFAULT '',
                kind          TEXT NOT NULL DEFAULT 'text',
                text          TEXT NOT NULL DEFAULT '',
                media_id      TEXT NOT NULL DEFAULT '',
                sender        TEXT NOT NULL DEFAULT '',
                sent_at_ms    INTEGER NOT NULL DEFAULT 0,
                created_at    INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_favorites_user ON favorites(user_id, created_at);
            "#;

/// P4 表：人机校验、恢复码、对账、举报工单、秘密聊天设置、审核挂起队列。
pub(super) const SCHEMA_P4: &str = r#"
            -- 人机校验（captcha）：只存答案摘要，一次性使用
            CREATE TABLE IF NOT EXISTS captcha (
                id          TEXT PRIMARY KEY,
                answer_hash TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER NOT NULL,
                used        INTEGER NOT NULL DEFAULT 0
            );

            -- 恢复码：一次性，摘要存储
            CREATE TABLE IF NOT EXISTS recovery_codes (
                user_id    TEXT PRIMARY KEY,
                code_hash  TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                used_at    INTEGER
            );

            -- 对账运行记录（每次 reconcile 一行，便于看趋势与追责）
            CREATE TABLE IF NOT EXISTS reconcile_runs (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms         INTEGER NOT NULL,
                users_checked INTEGER NOT NULL,
                wallet_sum    INTEGER NOT NULL,
                escrow        INTEGER NOT NULL,
                supply        INTEGER NOT NULL,
                funded_total  INTEGER NOT NULL,
                mismatches    INTEGER NOT NULL,
                detail        TEXT NOT NULL,
                ok            INTEGER NOT NULL
            );

            -- 举报工单（状态机在 wxlike-report，这里只持久化）
            CREATE TABLE IF NOT EXISTS reports (
                id           TEXT PRIMARY KEY,
                reporter     TEXT NOT NULL,
                target_kind  TEXT NOT NULL,
                target_id    TEXT NOT NULL,
                reason       TEXT NOT NULL,
                detail       TEXT NOT NULL,
                state        TEXT NOT NULL,
                action_json  TEXT,
                handler      TEXT,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL,
                history_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_reports_reporter ON reports(reporter, created_at);
            CREATE INDEX IF NOT EXISTS idx_reports_state ON reports(state, created_at);

            -- 秘密聊天：会话级消失消息 TTL（0/缺行 = 关闭）
            CREATE TABLE IF NOT EXISTS secret_settings (
                conv_id    TEXT PRIMARY KEY,
                enabled    INTEGER NOT NULL DEFAULT 1,
                ttl_ms     INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            );

            -- 秘密聊天：安全码核对状态（每会话每用户一行）
            CREATE TABLE IF NOT EXISTS secret_verifications (
                conv_id       TEXT NOT NULL,
                user_id       TEXT NOT NULL,
                peer_identity TEXT NOT NULL,
                verified_at   INTEGER NOT NULL,
                PRIMARY KEY (conv_id, user_id)
            );

            -- 先审后发：挂起待审的消息（审核通过后才允许投递）
            CREATE TABLE IF NOT EXISTS moderation_queue (
                id         TEXT PRIMARY KEY,
                conv_id    TEXT NOT NULL DEFAULT '',
                sender     TEXT NOT NULL,
                text       TEXT NOT NULL,
                labels     TEXT NOT NULL,
                decision   TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_modqueue_created ON moderation_queue(created_at);

            -- P5 直播会话（信令/弹幕独立于频道时间线，ADR-13）
            CREATE TABLE IF NOT EXISTS live_sessions (
                id           TEXT PRIMARY KEY,
                channel_id   TEXT NOT NULL,
                host         TEXT NOT NULL,
                stream_key   TEXT NOT NULL UNIQUE,
                state        TEXT NOT NULL,
                recording    INTEGER NOT NULL DEFAULT 0,
                started_at   INTEGER NOT NULL,
                ended_at     INTEGER,
                viewers      INTEGER NOT NULL DEFAULT 0,
                peak_viewers INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_live_channel ON live_sessions(channel_id, state);

            -- 在房观众：弹幕收件集来源
            CREATE TABLE IF NOT EXISTS live_viewers (
                session_id TEXT NOT NULL,
                user_id    TEXT NOT NULL,
                joined_at  INTEGER NOT NULL,
                PRIMARY KEY (session_id, user_id)
            );

            -- 视频转码回调落下的引用（见 `video_refs.rs`）：只存"能播了"这件事，
            -- 不存字节。`video_id` 是主键，重试回调覆盖同一行。
            CREATE TABLE IF NOT EXISTS video_refs (
                video_id      TEXT PRIMARY KEY,
                job_id        TEXT NOT NULL,
                state         TEXT NOT NULL,
                playlist_url  TEXT NOT NULL DEFAULT '',
                play_url      TEXT NOT NULL DEFAULT '',
                thumb_url     TEXT NOT NULL DEFAULT '',
                duration_ms   INTEGER NOT NULL DEFAULT 0,
                width         INTEGER NOT NULL DEFAULT 0,
                height        INTEGER NOT NULL DEFAULT 0,
                renditions    TEXT NOT NULL DEFAULT '[]',
                attempts      INTEGER NOT NULL DEFAULT 0,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_video_refs_job ON video_refs(job_id);
            "#;

/// G05：内容、社交、收藏与检索。表名按域加前缀，避免与平台既有表混淆。
pub(super) const SCHEMA_G05: &str = r#"
            -- G05-01 朋友圈草稿：发布前的文本/媒体/可见性可反复保存，
            -- 发布成功后客户端删除；表本身不含"已发布"状态。
            CREATE TABLE IF NOT EXISTS moment_drafts (
                id             TEXT PRIMARY KEY,
                author         TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text           TEXT NOT NULL DEFAULT '',
                media          TEXT NOT NULL DEFAULT '[]',
                visibility     TEXT NOT NULL DEFAULT 'friends',
                audience_group TEXT NOT NULL DEFAULT '',
                updated_at     INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_moment_drafts_author
                ON moment_drafts(author, updated_at);

            -- 内容 -> 媒体引用。删除内容时同步删行；用于回答"附件还被谁引用"
            -- 以及未来按引用计数清理孤儿对象。唯一约束让重复发布幂等。
            CREATE TABLE IF NOT EXISTS content_media_refs (
                media_id   TEXT NOT NULL,
                ref_kind   TEXT NOT NULL,
                ref_id     TEXT NOT NULL,
                owner      TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (ref_kind, ref_id, media_id)
            );
            CREATE INDEX IF NOT EXISTS idx_cmr_media ON content_media_refs(media_id);

            -- G05-02 发布者（一个用户一个发布者；不承诺导入微信后台）
            CREATE TABLE IF NOT EXISTS publishers (
                id              TEXT PRIMARY KEY,
                owner           TEXT NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
                name            TEXT NOT NULL,
                bio             TEXT NOT NULL DEFAULT '',
                avatar_media_id TEXT NOT NULL DEFAULT '',
                created_at      INTEGER NOT NULL
            );

            -- 文章：doc 是受限文档结构的 JSON，body_html 是服务端渲染结果。
            -- seq 同时是订阅未读游标（只增不减）。
            CREATE TABLE IF NOT EXISTS articles (
                seq          INTEGER PRIMARY KEY AUTOINCREMENT,
                id           TEXT NOT NULL UNIQUE,
                publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
                author       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                title        TEXT NOT NULL DEFAULT '',
                doc          TEXT NOT NULL DEFAULT '{}',
                body_text    TEXT NOT NULL DEFAULT '',
                body_html    TEXT NOT NULL DEFAULT '',
                state        TEXT NOT NULL DEFAULT 'draft',
                version      INTEGER NOT NULL DEFAULT 1,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL,
                published_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_articles_pub
                ON articles(publisher_id, seq);
            CREATE INDEX IF NOT EXISTS idx_articles_state
                ON articles(state, seq);

            CREATE TABLE IF NOT EXISTS article_versions (
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                version    INTEGER NOT NULL,
                doc        TEXT NOT NULL,
                saved_at   INTEGER NOT NULL,
                PRIMARY KEY (article_id, version)
            );

            -- 订阅：read_cursor = 已读到的文章 seq。未读 = 比它大的已发布文章。
            CREATE TABLE IF NOT EXISTS publisher_follows (
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                publisher_id TEXT NOT NULL REFERENCES publishers(id) ON DELETE CASCADE,
                read_cursor  INTEGER NOT NULL DEFAULT 0,
                created_at   INTEGER NOT NULL,
                PRIMARY KEY (user_id, publisher_id)
            );
            CREATE TABLE IF NOT EXISTS article_reads (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                read_at    INTEGER NOT NULL,
                PRIMARY KEY (user_id, article_id)
            );
            -- 在看/分享/收藏是三个独立动作，各自一行。
            CREATE TABLE IF NOT EXISTS article_reactions (
                article_id TEXT NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind       TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (article_id, user_id, kind)
            );

            -- G05-03 笔记：version 是乐观锁；写入必须带 expected_version。
            CREATE TABLE IF NOT EXISTS notes (
                id         TEXT PRIMARY KEY,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                title      TEXT NOT NULL DEFAULT '',
                body       TEXT NOT NULL DEFAULT '',
                version    INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_notes_user ON notes(user_id, updated_at);
            CREATE TABLE IF NOT EXISTS note_versions (
                note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                version  INTEGER NOT NULL,
                title    TEXT NOT NULL,
                body     TEXT NOT NULL,
                saved_at INTEGER NOT NULL,
                PRIMARY KEY (note_id, version)
            );

            -- 表情包：包级许可字段用于"版权"约束；安装是每用户独立行。
            CREATE TABLE IF NOT EXISTS sticker_packs (
                id         TEXT PRIMARY KEY,
                owner      TEXT NOT NULL DEFAULT '',
                name       TEXT NOT NULL,
                license    TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stickers (
                id         TEXT PRIMARY KEY,
                pack_id    TEXT NOT NULL REFERENCES sticker_packs(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                name       TEXT NOT NULL DEFAULT '',
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_stickers_pack ON stickers(pack_id, sort_order);
            CREATE TABLE IF NOT EXISTS user_stickers (
                user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                sticker_id   TEXT NOT NULL REFERENCES stickers(id) ON DELETE CASCADE,
                sort_order   INTEGER NOT NULL DEFAULT 0,
                installed_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, sticker_id)
            );

            -- G05-04 视频号：发布记录只是引用；能否播以 video_refs 的
            -- state='succeeded' 为准（转码门禁）。
            CREATE TABLE IF NOT EXISTS video_posts (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                id         TEXT NOT NULL UNIQUE,
                author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                caption    TEXT NOT NULL DEFAULT '',
                state      TEXT NOT NULL DEFAULT 'published',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_video_posts_author
                ON video_posts(author, seq);
            CREATE INDEX IF NOT EXISTS idx_video_posts_state
                ON video_posts(state, seq);
            CREATE TABLE IF NOT EXISTS video_likes (
                video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (video_id, user_id)
            );
            CREATE TABLE IF NOT EXISTS video_comments (
                seq        INTEGER PRIMARY KEY AUTOINCREMENT,
                id         TEXT NOT NULL UNIQUE,
                video_id   TEXT NOT NULL REFERENCES video_posts(id) ON DELETE CASCADE,
                author     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                text       TEXT NOT NULL,
                deleted    INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_video_comments
                ON video_comments(video_id, seq);
            CREATE TABLE IF NOT EXISTS video_follows (
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                creator    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, creator)
            );

            -- G05-05 公开内容索引：只放可被"公开检索"投影的字段。
            -- 写入与内容发布同事务、删除同步，所以不存在异步落后窗口。
            CREATE TABLE IF NOT EXISTS content_index (
                doc_id     TEXT PRIMARY KEY,
                kind       TEXT NOT NULL,
                owner      TEXT NOT NULL,
                audience   TEXT NOT NULL DEFAULT 'public',
                group_name TEXT NOT NULL DEFAULT '',
                title      TEXT NOT NULL DEFAULT '',
                body       TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_content_index_recent
                ON content_index(created_at DESC, doc_id DESC);
            CREATE INDEX IF NOT EXISTS idx_content_index_kind
                ON content_index(kind, created_at DESC);

            -- 通知收件箱：read_at 在服务端，跨端共享。"事务事件生成"由调用方
            -- 与业务写同事务插入；dedupe_key 保证重试/重复点赞不刷屏。
            CREATE TABLE IF NOT EXISTS notifications (
                seq         INTEGER PRIMARY KEY AUTOINCREMENT,
                id          TEXT NOT NULL UNIQUE,
                user_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind        TEXT NOT NULL,
                actor       TEXT NOT NULL DEFAULT '',
                target_kind TEXT NOT NULL DEFAULT '',
                target_id   TEXT NOT NULL DEFAULT '',
                text        TEXT NOT NULL DEFAULT '',
                dedupe_key  TEXT NOT NULL DEFAULT '',
                created_at  INTEGER NOT NULL,
                read_at     INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_notifications_user
                ON notifications(user_id, seq DESC);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_notifications_dedupe
                ON notifications(user_id, dedupe_key) WHERE dedupe_key <> '';

            -- G05-06 状态：到期由服务端 now 判定；cleared_at 保留审计/事件痕迹。
            CREATE TABLE IF NOT EXISTS statuses (
                id                  TEXT PRIMARY KEY,
                owner               TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                kind                TEXT NOT NULL,
                text                TEXT NOT NULL DEFAULT '',
                media_id            TEXT NOT NULL DEFAULT '',
                background_media_id TEXT NOT NULL DEFAULT '',
                audience            TEXT NOT NULL DEFAULT 'friends',
                version             INTEGER NOT NULL DEFAULT 1,
                created_at          INTEGER NOT NULL,
                updated_at          INTEGER NOT NULL,
                expires_at          INTEGER NOT NULL,
                cleared_at          INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_statuses_owner
                ON statuses(owner, updated_at DESC);
            CREATE TABLE IF NOT EXISTS status_background_drafts (
                owner      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
                media_id   TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS status_reactions (
                status_id  TEXT NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
                user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (status_id, user_id)
            );
            "#;
