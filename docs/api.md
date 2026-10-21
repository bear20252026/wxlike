# HTTP 契约片段

> 本文件按区累积 OpenAPI 片段（每片可单独喂 `openapi-cli validate`，也能拼成
> 一份完整 spec：`info` / `securitySchemes` 各片同名同值，合并时以任一片为准）。
> 服务端真相在 `crates/platform/src/*.rs` 的 `routes()` 与 handler；
> 命令面真相在 `crates/cmd::CATALOG`。
>
> 约定（全平台一致）：鉴权走 `Authorization: Bearer <token>`；错误体一律
> `{"error": "<面向客户端的一句话>"}`，SQL/IO 细节只进服务端日志；
> 写操作可用 `Idempotency-Key` 头重试。状态码语义：
> 401 未鉴权 · 403 无权限/非成员 · 404 不存在 · 409 冲突 ·
> 422 输入非法 · 428 需要人机校验 · 429 限流。
>
> **请求体解析失败也是这个形状**：`422 {"error": "请求体不合法（字段 name）：..."}`。
> 不用 axum 默认的 `Json`（它会返回纯文本），统一走
> `wxlike_platform::extract::ApiJson`；错误里带字段路径，调用方不用猜是哪一项。

## Zone C：社群层（社区 / 频道 / 话题）

来源：`crates/platform/src/communities.rs`（合同 §3.4，ADR-16 三支柱之三）。
设计与两例取舍见 [p3-communities.md](p3-communities.md)。

```yaml
openapi: 3.1.0
info:
  title: wxlike — Zone C (communities)
  version: 1.0.0
  license:
    name: AGPL-3.0-only
security:
  - bearerAuth: []
tags:
  - name: community
    description: 社区与成员（社区角色管建/删频道、踢人、提管理员）
  - name: channel
    description: 频道与话题（频道角色管能说什么、能不能撤他人消息）
paths:
  /api/communities:
    post:
      tags: [community]
      summary: 建社区（建者成为群主，role=1）
      operationId: createCommunity
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [name]
              properties:
                name: { type: string, maxLength: 64 }
                member_limit:
                  type: integer
                  description: 钳到 2..=100000；传 0 就是"只留一个名额"
      responses:
        "201":
          content:
            application/json:
              schema:
                type: object
                properties: { community_id: { type: string } }
        "422": { $ref: '#/components/responses/Unprocessable' }

  /api/communities/mine:
    get:
      tags: [community]
      summary: 我参与的社区（带我在其中的社区角色）
      operationId: myCommunities
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  communities:
                    type: array
                    items:
                      type: object
                      properties:
                        id: { type: string }
                        name: { type: string }
                        owner_uid: { type: string }
                        member_limit: { type: integer }
                        role:
                          type: integer
                          description: 0 成员 / 1 群主 / 2 管理员（**别用大小比较判强弱**）

  /api/communities/{id}/join:
    post:
      tags: [community]
      summary: 加入社区（幂等：已在里面返回 already_member）
      operationId: joinCommunity
      parameters:
        - { $ref: '#/components/parameters/communityId' }
      responses:
        "200":
          description: state = joined | already_member
          content:
            application/json:
              schema:
                type: object
                properties: { state: { type: string, enum: [joined, already_member] } }
        "404": { $ref: '#/components/responses/NotFound' }
        "422":
          description: 成员数已达 member_limit
          $ref: '#/components/responses/Unprocessable'

  /api/communities/{id}/members:
    get:
      tags: [community]
      summary: 社区成员与社区角色（成员自己就能看）
      operationId: communityMembers
      parameters:
        - { $ref: '#/components/parameters/communityId' }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  members:
                    type: array
                    items:
                      type: object
                      properties:
                        user_id: { type: string }
                        role: { type: integer }
        "403": { $ref: '#/components/responses/Forbidden' }

  /api/communities/{id}/channels:
    get:
      tags: [channel]
      summary: 社区下的频道列表（成员可见）
      operationId: listChannels
      parameters:
        - { $ref: '#/components/parameters/communityId' }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  channels:
                    type: array
                    items: { $ref: '#/components/schemas/Channel' }
    post:
      tags: [channel]
      summary: 建频道（要社区 manage-channel；同时把社区成员写进频道信箱快照）
      operationId: createChannel
      parameters:
        - { $ref: '#/components/parameters/communityId' }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [name, kind]
              properties:
                name: { type: string, maxLength: 64 }
                kind: { type: string, enum: [text, announce, voice, live] }
      responses:
        "201":
          content:
            application/json:
              schema:
                type: object
                properties: { channel_id: { type: string } }
        "403": { $ref: '#/components/responses/Forbidden' }
        "422": { $ref: '#/components/responses/Unprocessable' }

  /api/community/{id}/threads:
    get:
      tags: [channel]
      summary: 社区内全部话题（跨频道汇总，最新在前）
      description: >
        一个入口拿到"这个社区正在讨论什么"，客户端不必先枚举频道再逐个问。
        `subscribed` 是"我"的状态；没订阅也能在频道时间线里看到话题根消息。
        排序是 `(created_at DESC, id DESC)`：同一毫秒建的两个话题也要有确定次序，
        否则带 limit 翻页会出现"同一条在两页都出现"或"两页都不出现"。
      operationId: communityThreads
      parameters:
        - { $ref: '#/components/parameters/communityId' }
        - name: limit
          in: query
          schema: { type: integer, minimum: 1, maximum: 200, default: 50 }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  threads:
                    type: array
                    items: { $ref: '#/components/schemas/CommunityThread' }
        "403": { $ref: '#/components/responses/Forbidden' }
    post:
      tags: [channel]
      summary: 从社区入口开话题（必须点名频道）
      operationId: createThreadInCommunity
      parameters:
        - { $ref: '#/components/parameters/communityId' }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [channel_id, root_seq]
              properties:
                channel_id:
                  type: string
                  description: 必须属于路径里这个社区，否则 404（防跨社区写）
                root_seq: { type: integer, description: 频道时间线里的根消息 seq }
                title: { type: string, default: "" }
      responses:
        "201":
          content:
            application/json:
              schema:
                type: object
                properties: { thread_id: { type: string } }
        "403": { $ref: '#/components/responses/Forbidden' }
        "404": { $ref: '#/components/responses/NotFound' }
        "422": { $ref: '#/components/responses/Unprocessable' }

  /api/channels/{id}/messages:
    get:
      tags: [channel]
      summary: 频道时间线（读扩散信箱，owner=channel）
      operationId: fetchChannelMessages
      parameters:
        - { $ref: '#/components/parameters/channelId' }
        - name: after_seq
          in: query
          schema: { type: integer, default: 0 }
        - name: limit
          in: query
          schema: { type: integer, default: 50 }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  messages:
                    type: array
                    items: { $ref: '#/components/schemas/ChannelMessage' }
                  next_after_seq:
                    type: [integer, "null"]
                    description: 还有下一页才给；到头是 null
    post:
      tags: [channel]
      summary: 发一条频道消息（按消息类型 + 频道形态双重闸门）
      operationId: sendChannelMessage
      parameters:
        - { $ref: '#/components/parameters/channelId' }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [text]
              properties:
                text: { type: string, maxLength: 5000 }
                kind:
                  type: string
                  enum: [text, image, msg.file, msg.sticker, msg.voice, msg.video]
                  description: >
                    缺省 text。**白名单而不是黑名单**：system / msg.forward /
                    wallet.redpacket 这类卡片由服务端逻辑落库，放开端点等于让人
                    伪造成系统口吻发消息——不在名单里的一律 422。
                    附件类要 post-media（guest 没有这个点）；
                    announce 型频道还要额外持有 post-announce。
      responses:
        "200":
          description: msg_id 就是撤回要用的那个 id（只回 seq 等于让调用方猜）
          content:
            application/json:
              schema:
                type: object
                properties:
                  seq: { type: integer }
                  msg_id: { type: string }
        "403": { $ref: '#/components/responses/Forbidden' }
        "404": { $ref: '#/components/responses/NotFound' }
        "422": { $ref: '#/components/responses/Unprocessable' }

  /api/channels/{id}/messages/recall:
    post:
      tags: [channel]
      summary: 撤回一条频道消息（本人的自由；他人的要 delete-any-message）
      operationId: recallChannelMessage
      parameters:
        - { $ref: '#/components/parameters/channelId' }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [msg_id]
              properties: { msg_id: { type: string } }
      responses:
        "204": { description: 撤回成功（逻辑删除，时间线不塌缩） }
        "403": { description: 不是本人且没有 delete-any-message }
        "404": { $ref: '#/components/responses/NotFound' }

  /api/channels/{id}/threads:
    get:
      tags: [channel]
      summary: 一个频道下的话题
      operationId: listThreads
      parameters:
        - { $ref: '#/components/parameters/channelId' }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  threads:
                    type: array
                    items: { $ref: '#/components/schemas/Thread' }
    post:
      tags: [channel]
      summary: 开话题（要 create-thread；发起者自动订阅）
      operationId: createThread
      parameters:
        - { $ref: '#/components/parameters/channelId' }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [root_seq]
              properties:
                root_seq: { type: integer }
                title: { type: string, default: "" }
      responses:
        "201":
          content:
            application/json:
              schema:
                type: object
                properties: { thread_id: { type: string } }
        "422": { description: root_seq<=0 或超出频道时间线 head }

  /api/channel/{id}/roles:
    get:
      tags: [channel]
      summary: 频道角色名单（有效角色 + 是否继承；成员自己就能看）
      description: >
        每个社区成员都在列。没有专属行的显示为继承来的社区角色
        （owner→owner、admin→moderator、member→member）。
      operationId: listChannelRoles
      parameters:
        - { $ref: '#/components/parameters/channelId' }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  channel_id: { type: string }
                  my_role:
                    type: [string, "null"]
                    enum: [owner, moderator, member, guest, "null"]
                  roles:
                    type: array
                    items: { $ref: '#/components/schemas/ChannelRole' }
        "403": { $ref: '#/components/responses/Forbidden' }
    post:
      tags: [channel]
      summary: 覆盖某人的频道角色（要 manage-roles —— 只有频道 owner 有）
      operationId: setChannelRole
      parameters:
        - { $ref: '#/components/parameters/channelId' }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [user_id, role]
              properties:
                user_id: { type: string }
                role: { type: string, enum: [moderator, member, guest] }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  user_id: { type: string }
                  role: { type: string }
        "403":
          description: 授不动（非 owner），或目标是社区群主
          $ref: '#/components/responses/Forbidden'
        "422":
          description: role=owner（owner 来自社区群主，走转让不是授予）或非法角色名
          $ref: '#/components/responses/Unprocessable'

  /api/channel/{id}/roles/{user}:
    delete:
      tags: [channel]
      summary: 撤掉覆盖行 = 回到继承社区角色（不是把人踢出频道）
      operationId: resetChannelRole
      parameters:
        - { $ref: '#/components/parameters/channelId' }
        - { $ref: '#/components/parameters/userId' }
      responses:
        "204": { description: 已回到继承 }
        "403": { $ref: '#/components/responses/Forbidden' }

  /api/threads/{id}/messages:
    get:
      tags: [channel]
      summary: 话题内回复（能否读只看社区成员身份，与订阅无关）
      operationId: fetchThreadMessages
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
          description: thread id
        - name: after_seq
          in: query
          schema: { type: integer, default: 0 }
        - name: limit
          in: query
          schema: { type: integer, default: 100 }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  messages:
                    type: array
                    items:
                      type: object
                      properties:
                        seq: { type: integer }
                        sender: { type: string }
                        text: { type: string }
    post:
      tags: [channel]
      summary: 回复话题（落话题自持序列；推送只扇给订阅且未静音的人）
      operationId: sendThreadReply
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [text]
              properties: { text: { type: string, maxLength: 5000 } }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties: { seq: { type: integer } }

  /api/threads/{id}/subscribe:
    post:
      tags: [channel]
      summary: 订阅（不静音）
      operationId: subscribeThread
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      responses:
        "204": { description: 已订阅 }
    put:
      tags: [channel]
      summary: 一次写定订阅 + 静音状态（幂等）
      description: 静音的人**仍在订阅名单里**、仍能读，只是不收推送。
      operationId: setThreadSubscription
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties: { muted: { type: boolean, default: false } }
      responses:
        "204": { description: 状态已写入 }
    delete:
      tags: [channel]
      summary: 退订（从扇出名单掉出去，读面不受影响）
      operationId: unsubscribeThread
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      responses:
        "204": { description: 已退订 }

  /api/threads/{id}/subscribers:
    get:
      tags: [channel]
      summary: 订阅名单（含 muted）与**真正要推送的那批**
      description: >
        `fanout` = 订阅 ∩ 仍在社区 ∩ 未静音。名单交给服务端算，
        客户端各算一遍交集迟早漏推送。
      operationId: threadSubscribers
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      responses:
        "200":
          content:
            application/json:
              schema:
                type: object
                properties:
                  subscribers:
                    type: array
                    items:
                      type: object
                      properties:
                        user_id: { type: string }
                        muted: { type: boolean }
                  fanout:
                    type: array
                    items: { type: string }

components:
  securitySchemes:
    bearerAuth: { type: http, scheme: bearer, bearerFormat: opaque }
  parameters:
    communityId: { name: id, in: path, required: true, schema: { type: string } }
    channelId: { name: id, in: path, required: true, schema: { type: string } }
    userId: { name: user, in: path, required: true, schema: { type: string } }
  schemas:
    Channel:
      type: object
      properties:
        id: { type: string }
        community_id: { type: string }
        name: { type: string }
        kind: { type: string, enum: [text, announce, voice, live] }
        tenant_id: { type: string, description: 不变式 8 的预留列，恒为 default }
        created_at: { type: integer, format: int64 }
    Thread:
      type: object
      properties:
        id: { type: string }
        channel_id: { type: string }
        root_seq: { type: integer, description: 根消息在频道时间线里的 seq }
        title: { type: string }
        created_by: { type: string }
        created_at: { type: integer, format: int64 }
    CommunityThread:
      type: object
      properties:
        thread_id: { type: string }
        channel_id: { type: string }
        channel_name: { type: string }
        root_seq: { type: integer }
        title: { type: string }
        created_by: { type: string }
        created_at: { type: integer, format: int64 }
        replies: { type: integer }
        subscribed: { type: boolean }
    ChannelRole:
      type: object
      properties:
        user_id: { type: string }
        role: { type: string, enum: [owner, moderator, member, guest] }
        inherited:
          type: boolean
          description: true = 来自社区角色继承，库里没有专属行
        granted_by: { type: string, description: 覆盖时是谁授的 }
    ChannelMessage:
      type: object
      properties:
        seq: { type: integer }
        sender: { type: string }
        text: { type: string }
        kind: { type: string, description: 信箱里的 type_id }
        msg_id: { type: string, description: 撤回用的 id }
        withdrawn: { type: boolean }
  responses:
    Forbidden:
      description: 无权限 / 非成员（错误体不区分"没权限"和"不是成员"，避免探测名单）
      content:
        application/json:
          schema: { $ref: '#/components/schemas/Error' }
    NotFound:
      description: 不存在
      content:
        application/json:
          schema: { $ref: '#/components/schemas/Error' }
    Unprocessable:
      description: 输入非法
      content:
        application/json:
          schema: { $ref: '#/components/schemas/Error' }
    Error:
      type: object
      properties: { error: { type: string } }
```

### 命令面对照（`crates/cmd::CATALOG`）

新增功能 = 往 catalog 加一条 + 在一个入口实现。Zone C 加了 2 条（总数 18 → 20）：

| 命令 id | 能力 | 参数 | 落到 |
|---|---|---|---|
| `community.threads` | Read | `{community, limit?}` | `GET /api/community/{id}/threads` |
| `channel.roles` | Read | `{conv}`（conv=频道 id） | `GET /api/channel/{id}/roles` |

`channel.roles` 的 `needs_conv=true`：会话白名单（`WXLIKE_AGENT_ALLOW_CONV`）
管得住频道 id，agent 被限制在哪些会话里，就只能看那些频道的角色名单。

发现面（`wxlike-cmd` 是 catalog/policy 的自检工具，不发请求）：

```bash
cargo run -p wxlike-cmd -- list community     # 看到 community.threads
cargo run -p wxlike-cmd -- list 角色           # 中文说明也搜得到
WXLIKE_AGENT_ALLOW_CONV='*' cargo run -p wxlike-cmd -- policy
```

执行面（`wxlike-cli` / MCP 共用 `crates/client` 的执行器）：

```bash
cargo run -p wxlike-cli -- --server http://127.0.0.1:8099 --token "$T" \
  run --cmd 'community.threads={"community":"<id>","limit":20}'

WXLIKE_AGENT_ALLOW_CONV='<channel-id>' cargo run -p wxlike-cli -- \
  --server http://127.0.0.1:8099 --token "$T" \
  run --cmd 'channel.roles={"conv":"<channel-id>"}'
```

