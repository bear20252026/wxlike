/// 界面唯一能问的问题集合。
///
/// 每个 port 对应 `01_contracts_and_data.md` 里的一个域服务；实现由生成绑定
/// 提供（G01 生成、G12 挂到组合根），G13 只依赖这里的签名。签名里出现的东西
/// 都是契约要求的：幂等键、expected version、confirmation、operation_id、
/// cursor/snapshot_version。
library;

import 'core.dart';
import 'models.dart';

/// 精确金额格式化。必须是 Rust 实现：Dart 的 `double` 不能用来表示金额，
/// 手写 `/100` 会在负数、零位币种（JPY/KRW）上出错。
abstract class FormatPort {
  String money(Money value);
}

abstract class ContentPort {
  Future<PageResult<Moment>> moments({String cursor = '', int limit = 20});

  Future<Moment> publishMoment(
    MomentDraft draft, {
    required String idempotencyKey,
  });

  Future<Moment> setMomentLike(
    String momentId,
    bool liked, {
    required String idempotencyKey,
  });

  Future<MomentComment> commentMoment(
    String momentId,
    String text, {
    required String idempotencyKey,
  });

  Future<void> deleteMoment(String momentId, {required String idempotencyKey});

  Future<PageResult<FavoriteItem>> favorites({
    String cursor = '',
    String type = '',
    int limit = 20,
  });

  Future<FavoriteItem> addFavorite({
    required String type,
    required String sourceId,
    required String note,
    required String idempotencyKey,
  });

  Future<void> removeFavorite(
    String favoriteId, {
    required String idempotencyKey,
  });

  Future<PageResult<StickerPack>> stickerPacks({
    String cursor = '',
    int limit = 20,
  });

  Future<StickerPack> downloadStickerPack(
    String packId, {
    required String idempotencyKey,
  });
}

abstract class PublisherPort {
  /// `query` 非空时由服务端做名称/文章检索，界面不做本地过滤冒充搜索。
  Future<PageResult<Publisher>> publishers({
    String query = '',
    String cursor = '',
    int limit = 20,
  });

  Future<Publisher> setSubscribed(
    String publisherId,
    bool subscribed, {
    required int expectedVersion,
    required String idempotencyKey,
  });

  Future<PageResult<Article>> articles(
    String publisherId, {
    String cursor = '',
    int limit = 20,
  });

  /// 正文必须按列表拿到的 `docVersion` 取；版本不一致由服务端拒绝，
  /// 界面重新拉列表，不能拿旧正文硬配新卡片。
  Future<Article> article(String articleId, {required int docVersion});
}

abstract class VideoPort {
  Future<PageResult<VideoItem>> feed({String cursor = '', int limit = 20});

  Future<VideoItem> video(String videoId);

  /// 播放器只允许同时存在一个；开启新流前由调用方释放旧的。
  Future<String> playUrl(String videoId);

  /// 离页/切换时释放解码器与音频焦点；返回后不得再收到该视频的帧。
  Future<void> stopPlayback();
}

abstract class LivePort {
  Future<LiveSession> session(String channel);

  Future<LiveSession> start(
    String channel,
    String title, {
    required String idempotencyKey,
  });

  Future<LiveSession> stop(String channel, {required String idempotencyKey});

  Future<LiveSession> join(String channel, {required String idempotencyKey});

  Future<LiveSession> leave(String channel, {required String idempotencyKey});

  Future<void> danmaku(
    String channel,
    String text, {
    required String idempotencyKey,
  });

  /// 直播/通话等长状态变化来自服务端事件，不是本地计时器。
  Stream<LiveSession> updates(String channel);
}

abstract class CallPort {
  /// 当前是否有通话（来电、进行中、最小化）。恢复界面据此重建状态。
  Future<CallSnapshot?> current();

  Stream<CallSnapshot> updates();

  Future<CallSnapshot> start(
    String convId, {
    required bool video,
    required String idempotencyKey,
  });

  Future<CallSnapshot> answer(String callId, {required String idempotencyKey});

  Future<CallSnapshot> hangUp(String callId, {required String idempotencyKey});

  Future<CallSnapshot> setMuted(
    String callId,
    bool muted, {
    required String idempotencyKey,
  });

  Future<CallSnapshot> setSpeaker(
    String callId,
    bool speaker, {
    required String idempotencyKey,
  });

  Future<CallSnapshot> setCamera(
    String callId,
    bool on, {
    required String idempotencyKey,
  });

  Future<CallSnapshot> setBluetooth(
    String callId,
    bool on, {
    required String idempotencyKey,
  });

  Future<CallSnapshot> flipCamera(
    String callId, {
    required String idempotencyKey,
  });

  Future<CallSnapshot> setMinimized(String callId, bool minimized);

  /// 挂断/离页后释放摄像头麦克风；返回后硬件指示必须已关闭。
  Future<void> releaseMedia();
}

abstract class CommercePort {
  Future<WalletAccount> wallet();

  Future<PageResult<BillEntry>> bills({String cursor = '', int limit = 20});

  Future<List<BankCard>> bankCards();

  Future<BankCard> addBankCard({
    required String cardToken,
    required String idempotencyKey,
  });

  Future<void> removeBankCard(String cardId, {required String idempotencyKey});

  /// 转账先 prepare 拿确认票据，再 execute；不能 prepare 完直接当成功。
  Future<PaymentIntent> prepareTransfer(
    TransferDraft draft, {
    required String idempotencyKey,
  });

  Future<PaymentIntent> executePayment(
    String intentId, {
    required String idempotencyKey,
  });

  Future<PaymentIntent> paymentIntent(String intentId);

  Future<RedPacket> redPacket(String packetId);

  Future<RedPacket> createRedPacket({
    required String convId,
    required Money total,
    required int count,
    required String greeting,
    required String idempotencyKey,
  });

  Future<RedPacket> claimRedPacket(
    String packetId, {
    required String idempotencyKey,
  });

  Future<PageResult<CardEntry>> entitlements({
    String cursor = '',
    int limit = 20,
  });

  Future<CardEntry> redeem(
    String entitlementId, {
    required String merchantToken,
    required String idempotencyKey,
  });

  /// 标准可扫码的收/付款码载荷；码图本身由平台桥渲染，业务层只给载荷。
  /// `kind` ∈ `pay` / `receive`。
  Future<String> paymentCode({
    required String kind,
    required String idempotencyKey,
  });

  Future<void> saveQrToAlbum(String payload, {required String idempotencyKey});
}

abstract class DiscoveryPort {
  /// 从一张已解码的图片/相机帧引用做安全深链解析。相机本身由平台桥提供。
  Future<ScanResult> decodeScan(String imageRef);

  Future<RecognitionResult> recognize(
    String imageRef, {
    required String targetLanguage,
  });

  Future<RecognitionResult> translate(
    String text, {
    required String targetLanguage,
  });

  Future<PageResult<NearbyPlace>> nearby({
    required String cell,
    String cursor = '',
    int limit = 20,
  });

  Future<PageResult<ServiceItem>> services({
    String category = '',
    String cursor = '',
    int limit = 20,
  });

  Future<ServiceQuote> quote(String serviceId);

  /// 报价之后创建供应商订单；返回长操作句柄，履约状态以服务端为准。
  Future<OperationHandle> createOrder(
    String quoteId, {
    required String idempotencyKey,
  });

  Future<OperationHandle> order(String operationId);
}

abstract class MiniPort {
  Future<PageResult<MiniAppInfo>> recent({String cursor = '', int limit = 20});

  Future<PageResult<MiniAppInfo>> catalog({String cursor = '', int limit = 20});

  Future<MiniAppInfo> install(String appId, {required String idempotencyKey});

  Future<List<String>> grantedScopes(String appId);

  /// 启动需要真实包、验签与授权；返回的 operation 仅表示启动流程，
  /// 不能仅凭它宣称小程序可用。
  Future<OperationHandle> launch(
    String appId, {
    required String idempotencyKey,
  });

  /// 查询启动/安装作业状态；不能只凭 operation_id 查询别人的作业。
  Future<OperationHandle> operation(String operationId);
}

abstract class SearchPort {
  Future<PageResult<SearchHit>> search(
    String query, {
    String type = '',
    String cursor = '',
    int limit = 20,
  });

  Future<List<String>> history();

  Future<void> clearHistory({required String idempotencyKey});
}

abstract class ProfilePort {
  Future<ProfileInfo> profile();

  Future<ProfileInfo> updateProfile({
    required String name,
    required String signature,
    required int expectedVersion,
    required String idempotencyKey,
  });

  Future<SecurityInfo> security();

  Future<StorageUsage> storage();

  Future<StorageUsage> clearCache({required String idempotencyKey});

  Future<OperationHandle> exportBackup({required String idempotencyKey});

  Future<BackupFile> backupResult(String operationId);

  Future<void> restoreBackup(
    String fileId, {
    required String verificationDigest,
    required String idempotencyKey,
  });
}

abstract class PreferencesPort {
  Future<Preferences> load();

  Future<Preferences> update(
    Preferences next, {
    required String idempotencyKey,
  });
}

abstract class StatusPort {
  Future<UserStatus?> myStatus();

  Future<PageResult<UserStatus>> friendStatuses({
    String cursor = '',
    int limit = 20,
  });

  Future<UserStatus> publishStatus({
    required String text,
    required String background,
    required String visibility,
    required Duration ttl,
    required String idempotencyKey,
  });

  Future<void> clearStatus({required String idempotencyKey});
}

abstract class LocationPort {
  Future<LocationShare?> currentPlace(String convId);

  Future<PageResult<NearbyPlace>> places(
    String query, {
    String cursor = '',
    int limit = 20,
  });

  Future<LocationShare> sendLocation(
    String convId,
    String placeId, {
    required String idempotencyKey,
  });

  Future<LocationShare> startSharing(
    String convId, {
    required int minutes,
    required String idempotencyKey,
  });

  Future<void> stopSharing(String shareId, {required String idempotencyKey});

  Future<List<LocationShare>> activeShares(String convId);
}

abstract class QrPort {
  Future<MyQrCode> myCode(QrStyle style, {required String idempotencyKey});

  Future<MyQrCode> rotate({required String idempotencyKey});

  Future<void> saveToAlbum(String payload, {required String idempotencyKey});
}

/// 组合根交给页面的一整包端口。页面只拿自己需要的那几个，但导航时整包传递，
/// 避免每个页面各自去摸全局单例。
class G13Ports {
  const G13Ports({
    required this.format,
    required this.content,
    required this.publisher,
    required this.video,
    required this.live,
    required this.calls,
    required this.commerce,
    required this.discovery,
    required this.mini,
    required this.search,
    required this.profile,
    required this.preferences,
    required this.status,
    required this.location,
    required this.qr,
  });

  final FormatPort format;
  final ContentPort content;
  final PublisherPort publisher;
  final VideoPort video;
  final LivePort live;
  final CallPort calls;
  final CommercePort commerce;
  final DiscoveryPort discovery;
  final MiniPort mini;
  final SearchPort search;
  final ProfilePort profile;
  final PreferencesPort preferences;
  final StatusPort status;
  final LocationPort location;
  final QrPort qr;
}
