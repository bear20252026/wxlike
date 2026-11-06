//! 订单/报价/幂等键的仓储层。
//!
//! [`MemoryStore`] 只用于供应商故障注入与上游卡未合入时的并行开发；最终集成
//! 验收必须走 [`FileStore`]——它把订单事实写到进程外（崩溃/重启后可恢复），
//! 所有文件 IO 都在阻塞线程池执行（ADR-28），绝不在 async 上下文里同步读写盘。
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Result, ServiceError, ServiceErrorCode as Code};
use crate::order::Order;
use crate::quote::Quote;

/// 幂等键最长保留文件名安全哈希；原始键只落在记录里，不进路径。
const MAX_DOC_ID_LEN: usize = 96;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct IdemRecord {
    key: String,
    hash: String,
    done_order_id: Option<String>,
}

/// 订单、报价与幂等键的持久化端口。
///
/// 实现必须保证：`reserve_idem` 对同一 key 的并发调用只有一个赢；
/// `save_order` 为覆盖写（状态机推进）；读不到的行返回约定错误码而不是 panic。
#[async_trait]
pub trait OrderStore: Send + Sync {
    async fn insert_quote(&self, quote: &Quote) -> Result<()>;
    async fn get_quote(&self, quote_id: &str) -> Result<Quote>;
    async fn purge_expired(&self, now_ms: i64) -> usize;

    async fn save_order(&self, order: &Order) -> Result<()>;
    async fn get_order(&self, order_id: &str) -> Result<Option<Order>>;
    async fn find_order_by_quote(&self, actor: &str, quote_id: &str) -> Result<Option<Order>>;

    /// 返回 `Ok(Some(order_id))` 表示同键同请求已定稿；`Ok(None)` 表示本次抢到执行权；
    /// `Err(Conflict)` 表示同键不同请求或有并发请求在途。
    async fn reserve_idem(&self, key: &str, hash: &str) -> Result<Option<String>>;
    async fn commit_idem(&self, key: &str, hash: &str, order_id: &str) -> Result<()>;
    async fn abort_idem(&self, key: &str) -> Result<()>;
}

fn internal(what: &str) -> ServiceError {
    ServiceError::new(Code::Internal, what)
}

/// 阻塞线程池执行同步文件 IO；任务 panic/取消视为内部错误，不伪装成参数问题。
async fn offload<F, T>(label: &'static str, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| internal(label))
        .and_then(std::convert::identity)
}

// ---------------------------------------------------------------- 内存实现

#[derive(Default)]
pub struct MemoryStore {
    quotes: Mutex<HashMap<String, Quote>>,
    orders: Mutex<HashMap<String, Order>>,
    idem: Mutex<HashMap<String, IdemRecord>>,
}

#[async_trait]
impl OrderStore for MemoryStore {
    async fn insert_quote(&self, quote: &Quote) -> Result<()> {
        self.quotes
            .lock()
            .map_err(|_| internal("quote state"))?
            .insert(quote.quote_id.clone(), quote.clone());
        Ok(())
    }

    async fn get_quote(&self, quote_id: &str) -> Result<Quote> {
        self.quotes
            .lock()
            .map_err(|_| internal("quote state"))?
            .get(quote_id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::Expired, "quote not found or expired"))
    }

    async fn purge_expired(&self, now_ms: i64) -> usize {
        let Ok(mut quotes) = self.quotes.lock() else {
            return 0;
        };
        let before = quotes.len();
        quotes.retain(|_, quote| quote.expires_at_ms > now_ms);
        before - quotes.len()
    }

    async fn save_order(&self, order: &Order) -> Result<()> {
        self.orders
            .lock()
            .map_err(|_| internal("order state"))?
            .insert(order.order_id.clone(), order.clone());
        Ok(())
    }

    async fn get_order(&self, order_id: &str) -> Result<Option<Order>> {
        Ok(self
            .orders
            .lock()
            .map_err(|_| internal("order state"))?
            .get(order_id)
            .cloned())
    }

    async fn find_order_by_quote(&self, actor: &str, quote_id: &str) -> Result<Option<Order>> {
        Ok(self
            .orders
            .lock()
            .map_err(|_| internal("order state"))?
            .values()
            .find(|order| order.actor == actor && order.quote_id == quote_id)
            .cloned())
    }

    async fn reserve_idem(&self, key: &str, hash: &str) -> Result<Option<String>> {
        let mut entries = self.idem.lock().map_err(|_| internal("idem state"))?;
        match entries.get(key) {
            Some(record) if record.hash == hash && record.done_order_id.is_some() => {
                Ok(record.done_order_id.clone())
            }
            Some(_) => Err(ServiceError::new(Code::Conflict, "idempotency key reused")),
            None => {
                entries.insert(
                    key.to_owned(),
                    IdemRecord {
                        key: key.to_owned(),
                        hash: hash.to_owned(),
                        done_order_id: None,
                    },
                );
                Ok(None)
            }
        }
    }

    async fn commit_idem(&self, key: &str, hash: &str, order_id: &str) -> Result<()> {
        self.idem
            .lock()
            .map_err(|_| internal("idem state"))?
            .insert(
                key.to_owned(),
                IdemRecord {
                    key: key.to_owned(),
                    hash: hash.to_owned(),
                    done_order_id: Some(order_id.to_owned()),
                },
            );
        Ok(())
    }

    async fn abort_idem(&self, key: &str) -> Result<()> {
        self.idem
            .lock()
            .map_err(|_| internal("idem state"))?
            .remove(key);
        Ok(())
    }
}

// ---------------------------------------------------------------- 落盘实现

/// 真实仓储：每个实体一个 JSON 文档，写入走「临时文件 + fsync + 原子改名」。
///
/// 报价与订单是钱与承诺的事实，只增改不删（过期报价由 purge 显式清理）；
/// 幂等键以原始键的 SHA-256 命名，避免把用户可猜的键直接铺进路径。
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    /// 建目录并返回仓储句柄；目录不可用是配置/依赖问题，报 DependencyUnavailable。
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let probe = root.clone();
        offload("mkdir failed", move || {
            for part in ["orders", "quotes", "idem"] {
                fs::create_dir_all(probe.join(part)).map_err(|err| dependency(&probe, err))?;
            }
            Ok(())
        })
        .await?;
        Ok(Self { root })
    }

    fn doc_path(dir: &str, id: &str) -> PathBuf {
        Path::new(dir).join(format!("{id}.json"))
    }

    fn idem_path(&self, key: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        self.root
            .join("idem")
            .join(format!("{}.json", hex::encode(hasher.finalize())))
    }
}

fn dependency(path: &Path, err: io::Error) -> ServiceError {
    ServiceError::new(
        Code::DependencyUnavailable,
        format!("store io {}: {err}", path.display()),
    )
}

/// 原子覆盖写：先写同目录临时文件并 fsync，再 rename 顶替旧文档。
fn write_doc(root: &Path, rel: &Path, bytes: &[u8]) -> Result<()> {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|err| dependency(parent, err))?;
    }
    let tmp = full.with_extension(format!("{}.tmp", rand::random::<u32>()));
    let mut file = fs::File::create(&tmp).map_err(|err| dependency(&tmp, err))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|err| dependency(&tmp, err))?;
    drop(file);
    fs::rename(&tmp, &full).map_err(|err| dependency(&full, err))
}

fn read_doc<T: serde::de::DeserializeOwned>(root: &Path, rel: &Path) -> Result<Option<T>> {
    let full = root.join(rel);
    match fs::read(&full) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| internal("corrupt store document")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(dependency(&full, err)),
    }
}

fn read_json_at<T: serde::de::DeserializeOwned>(full: &Path) -> Result<Option<T>> {
    match fs::read(full) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| internal("corrupt store document")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(dependency(full, err)),
    }
}

/// 文档 id 由本域生成（前缀+128bit 随机 hex）；入库前仍复核，挡住被篡改的路径成分。
fn check_doc_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > MAX_DOC_ID_LEN
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ServiceError::new(Code::InvalidArgument, "document id"));
    }
    Ok(())
}

#[async_trait]
impl OrderStore for FileStore {
    async fn insert_quote(&self, quote: &Quote) -> Result<()> {
        check_doc_id(&quote.quote_id)?;
        let root = self.root.clone();
        let doc = quote.clone();
        let rel = Self::doc_path("quotes", &doc.quote_id);
        offload("quote write failed", move || {
            let bytes = serde_json::to_vec(&doc).map_err(|_| internal("quote encode"))?;
            write_doc(&root, &rel, &bytes)
        })
        .await
    }

    async fn get_quote(&self, quote_id: &str) -> Result<Quote> {
        check_doc_id(quote_id)?;
        let root = self.root.clone();
        let rel = Self::doc_path("quotes", quote_id);
        offload("quote read failed", move || {
            read_doc::<Quote>(&root, &rel)?
                .ok_or_else(|| ServiceError::new(Code::Expired, "quote not found or expired"))
        })
        .await
    }

    async fn purge_expired(&self, now_ms: i64) -> usize {
        let root = self.root.clone();
        let ran = tokio::task::spawn_blocking(move || -> Result<usize> {
            let dir = root.join("quotes");
            let Ok(entries) = fs::read_dir(&dir) else {
                return Ok(0);
            };
            let mut removed = 0usize;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let Ok(Some(quote)) = read_json_at::<Quote>(&path) else {
                    continue;
                };
                if quote.expires_at_ms <= now_ms && fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await;
        match ran {
            Ok(result) => result.unwrap_or(0),
            Err(_) => 0,
        }
    }

    async fn save_order(&self, order: &Order) -> Result<()> {
        check_doc_id(&order.order_id)?;
        let root = self.root.clone();
        let doc = order.clone();
        let rel = Self::doc_path("orders", &doc.order_id);
        offload("order write failed", move || {
            let bytes = serde_json::to_vec(&doc).map_err(|_| internal("order encode"))?;
            write_doc(&root, &rel, &bytes)
        })
        .await
    }

    async fn get_order(&self, order_id: &str) -> Result<Option<Order>> {
        check_doc_id(order_id)?;
        let root = self.root.clone();
        let rel = Self::doc_path("orders", order_id);
        offload("order read failed", move || read_doc::<Order>(&root, &rel)).await
    }

    async fn find_order_by_quote(&self, actor: &str, quote_id: &str) -> Result<Option<Order>> {
        check_doc_id(quote_id)?;
        let root = self.root.clone();
        let actor = actor.to_owned();
        let quote_id = quote_id.to_owned();
        offload("order scan failed", move || {
            let dir = root.join("orders");
            let entries = fs::read_dir(&dir).map_err(|err| dependency(&dir, err))?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(Some(order)) = read_json_at::<Order>(&path) {
                    if order.actor == actor && order.quote_id == quote_id {
                        return Ok(Some(order));
                    }
                }
            }
            Ok(None)
        })
        .await
    }

    async fn reserve_idem(&self, key: &str, hash: &str) -> Result<Option<String>> {
        let root = self.root.clone();
        let path = self.idem_path(key);
        let key = key.to_owned();
        let hash = hash.to_owned();
        offload("idem reserve failed", move || {
            fs::create_dir_all(path.parent().unwrap_or(Path::new(&root)))
                .map_err(|err| dependency(&root, err))?;
            let record = IdemRecord {
                key: key.clone(),
                hash: hash.clone(),
                done_order_id: None,
            };
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let bytes = serde_json::to_vec(&record).map_err(|_| internal("idem encode"))?;
                    file.write_all(&bytes)
                        .and_then(|_| file.sync_all())
                        .map_err(|err| dependency(&path, err))?;
                    Ok(None)
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    Err(internal("idem reserve raced"))
                }
                Err(_) => {
                    let existing: IdemRecord =
                        read_json_at(&path)?.ok_or_else(|| internal("idem record vanished"))?;
                    if existing.hash != hash {
                        return Err(ServiceError::new(Code::Conflict, "idempotency key reused"));
                    }
                    // 同键同请求：只有已定稿才允许复用结果；在途一律视为冲突。
                    existing.done_order_id.map_or(
                        Err(ServiceError::new(Code::Conflict, "idempotency key reused")),
                        |order_id| Ok(Some(order_id)),
                    )
                }
            }
        })
        .await
    }

    async fn commit_idem(&self, key: &str, hash: &str, order_id: &str) -> Result<()> {
        check_doc_id(order_id)?;
        let path = self.idem_path(key);
        let key = key.to_owned();
        let hash = hash.to_owned();
        let order_id = order_id.to_owned();
        let root = self.root.clone();
        offload("idem commit failed", move || {
            let record = IdemRecord {
                key,
                hash,
                done_order_id: Some(order_id),
            };
            let bytes = serde_json::to_vec(&record).map_err(|_| internal("idem encode"))?;
            // commit 直接覆盖在途记录：幂等定稿必须先于任何读取方看到结果。
            let tmp = path.with_extension("committed");
            let mut file = fs::File::create(&tmp).map_err(|err| dependency(&tmp, err))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|err| dependency(&tmp, err))?;
            drop(file);
            fs::rename(&tmp, &path).map_err(|err| dependency(path.parent().unwrap_or(&root), err))
        })
        .await
    }

    async fn abort_idem(&self, key: &str) -> Result<()> {
        let path = self.idem_path(key);
        offload("idem abort failed", move || match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(dependency(&path, err)),
        })
        .await
    }
}
