//! 秒传（内容寻址快路径）的验收。
//!
//! `media_object.object_id` 就是摘要本身，所以"这个对象已经存在"是一次按主键的
//! 查询。这里的重点不是"能跳过上传"，而是**快路径建出来的东西必须和正常路径等价**：
//! 自己的 `media_file` 行、自己的 `media_ref`（于是 `authorize_read` 过）、
//! 不重复落字节、不转移对象归属。反向用例同样重要：三条命中条件里任何一条不成立
//! （大小对不上、store 里字节没了），必须退回正常上传，而不是假装秒传成功。

use std::path::Path;
use std::sync::Arc;

use wxlike_media::*;

struct NoConversations;

impl AudiencePort for NoConversations {
    fn may_read_conversation(&self, _user_id: &str, _conv_id: &str) -> bool {
        false
    }

    fn may_read_group(&self, _user_id: &str, _group_id: &str) -> bool {
        false
    }
}

fn service_at(root: &Path) -> MediaService {
    MediaService::open(MediaConfig::new(root), None, Arc::new(NoConversations)).unwrap()
}

fn start(
    service: &MediaService,
    owner: &str,
    name: &str,
    bytes: &[u8],
    size: u64,
) -> UploadSession {
    service
        .upload_start(
            owner,
            UploadStart {
                purpose: Purpose::File,
                name: name.into(),
                size,
                mime: "application/octet-stream".into(),
                digest: ids::sha256_hex(bytes),
            },
        )
        .unwrap()
}

fn upload_full(service: &MediaService, owner: &str, name: &str, bytes: &[u8]) -> CompletedUpload {
    let session = start(service, owner, name, bytes, bytes.len() as u64);
    let step = session.chunk_size as usize;
    for (index, chunk) in bytes.chunks(step).enumerate() {
        service
            .upload_chunk(
                owner,
                &session.upload_id,
                (index * step) as u64,
                chunk,
                Some(&ids::sha256_hex(chunk)),
            )
            .unwrap();
    }
    service
        .upload_complete(owner, &session.upload_id, Some(&ids::sha256_hex(bytes)))
        .unwrap()
}

#[test]
fn instant_hit_completes_without_any_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let bytes = vec![7u8; 4096];
    let first = upload_full(&service, "u1", "a.bin", &bytes);
    assert!(first.newly_stored, "首次上传应当真的落一份字节");

    let second = start(&service, "u2", "copy.bin", &bytes, bytes.len() as u64);
    assert_eq!(
        second.status,
        UploadStatus::Completed,
        "摘要已就绪的第二个人不该还停在 open"
    );
    assert_eq!(second.object_id.as_deref(), Some(first.object_id.as_str()));

    // 会话既然已经完结，分片入口就得关上：往里再塞字节没有意义
    let rejected = service.upload_chunk("u2", &second.upload_id, 0, &bytes[..16], None);
    assert!(rejected.is_err(), "秒传命中的会话不该还接收分片");

    let done = service
        .upload_complete("u2", &second.upload_id, None)
        .unwrap();
    assert!(!done.newly_stored, "秒传不该把同一份字节再存一遍");
    assert_eq!(done.object_id, first.object_id);
    assert_ne!(done.file_id, first.file_id, "每个上传者要拿到自己的文件行");

    // 引用真的建了：u2 能过读授权，说明 media_ref 里有属于他这一条
    let grant = service.authorize_read("u2", &first.object_id).unwrap();
    assert_eq!(grant.object.size, bytes.len() as u64);
    // 去重不转移归属，对象行仍然是第一个把它放进来的人的
    assert_eq!(service.object(&first.object_id).unwrap().owner, "u1");
}

#[test]
fn instant_hit_stays_completed_when_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let bytes = vec![9u8; 1024];
    let first = upload_full(&service, "u1", "a.bin", &bytes);

    let second = start(&service, "u2", "copy.bin", &bytes, bytes.len() as u64);
    let after_cancel = service.upload_cancel("u2", &second.upload_id).unwrap();
    assert_eq!(
        after_cancel.status,
        UploadStatus::Completed,
        "已完结的秒传会话不该被 cancel 改写成 cancelled——文件行与引用已经存在"
    );
    assert!(service.authorize_read("u2", &first.object_id).is_ok());
}

#[test]
fn declared_size_mismatch_falls_back_to_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let bytes = vec![3u8; 2048];
    upload_full(&service, "u1", "a.bin", &bytes);

    // 同一个摘要、声明大小不一致：这不是同一个人声明的东西，不许秒传
    let session = start(&service, "u2", "other.bin", &bytes, bytes.len() as u64 + 1);
    assert_eq!(session.status, UploadStatus::Open);
    assert_eq!(session.object_id, None);
    service
        .upload_chunk(
            "u2",
            &session.upload_id,
            0,
            &bytes,
            Some(&ids::sha256_hex(&bytes)),
        )
        .unwrap();
    // 它是一条**真的**正常会话：字节收下了，但因为与声明大小不符而无法完结
    let error = service
        .upload_complete("u2", &session.upload_id, Some(&ids::sha256_hex(&bytes)))
        .unwrap_err();
    assert!(
        matches!(error, MediaError::Conflict(_)),
        "声明大小对不上就该停在未完成，实际 {error:?}"
    );
}

#[test]
fn missing_stored_bytes_fall_back_to_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let bytes = vec![5u8; 3000];
    let digest = ids::sha256_hex(&bytes);
    upload_full(&service, "u1", "a.bin", &bytes);
    assert!(service.open_object(&digest).unwrap().is_some());

    // 把媒体仓里的字节抹掉，只留 DB 行：这正是"行在、东西不在"的撕裂状态
    std::fs::remove_file(dir.path().join(&digest[..2]).join(&digest)).unwrap();
    assert!(service.open_object(&digest).unwrap().is_none());

    let session = start(&service, "u2", "copy.bin", &bytes, bytes.len() as u64);
    assert_eq!(
        session.status,
        UploadStatus::Open,
        "store 取不出字节时不许把会话落成 completed，否则引用挂在空对象上"
    );
}

#[test]
fn unknown_digest_starts_a_normal_session() {
    let dir = tempfile::tempdir().unwrap();
    let service = service_at(dir.path());
    let bytes = vec![1u8; 512];
    let session = start(&service, "u1", "new.bin", &bytes, bytes.len() as u64);
    assert_eq!(session.status, UploadStatus::Open);
    assert_eq!(session.declared_size, 512);
    let done = upload_full(&service, "u1", "new.bin", &bytes);
    assert!(done.newly_stored);
}
