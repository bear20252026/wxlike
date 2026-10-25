/// G13 页面只认这些视图模型；它们与域服务返回的字段一一对应，不含本地派生
/// 业务（未读、可领取、过期与否都必须由服务端算好）。
library;

import 'core.dart';

// ---------------------------------------------------------------------------
// 内容 / 收藏 / 公众号 / 表情
// ---------------------------------------------------------------------------

class MomentComment {
  const MomentComment({
    required this.id,
    required this.authorName,
    required this.text,
    required this.createdAt,
  });

  final String id;
  final String authorName;
  final String text;
  final DateTime createdAt;
}

class Moment {
  const Moment({
    required this.id,
    required this.authorName,
    required this.text,
    required this.createdAt,
    required this.media,
    required this.liked,
    required this.likeCount,
    required this.comments,
    required this.audience,
    required this.isMine,
  });

  final String id;
  final String authorName;
  final String text;
  final DateTime createdAt;
  final List<MediaRef> media;
  final bool liked;
  final int likeCount;
  final List<MomentComment> comments;

  /// 服务端判定的可见范围；界面只显示，不自行推断。
  final String audience;

  /// 是否本人发布（删除入口只对自己出现）。由服务端算，不比较昵称。
  final bool isMine;

  Moment copyWith({
    bool? liked,
    int? likeCount,
    List<MomentComment>? comments,
  }) => Moment(
    id: id,
    authorName: authorName,
    text: text,
    createdAt: createdAt,
    media: media,
    liked: liked ?? this.liked,
    likeCount: likeCount ?? this.likeCount,
    comments: comments ?? this.comments,
    audience: audience,
    isMine: isMine,
  );
}

class MomentDraft {
  const MomentDraft({
    required this.text,
    required this.mediaIds,
    required this.audience,
  });

  final String text;
  final List<String> mediaIds;
  final String audience;
}

class ArticleBlock {
  const ArticleBlock({
    required this.kind,
    required this.text,
    this.caption = '',
  });

  /// `heading` / `paragraph` / `image`。未知类型按 paragraph 渲染，
  /// 不因为服务端新增块类型就整篇打不开。
  final String kind;
  final String text;
  final String caption;
}

class Article {
  const Article({
    required this.id,
    required this.publisherId,
    required this.title,
    required this.summary,
    required this.author,
    required this.publishedAt,
    required this.docVersion,
    required this.blocks,
    required this.revoked,
  });

  final String id;
  final String publisherId;
  final String title;
  final String summary;
  final String author;
  final DateTime publishedAt;

  /// 文章净化结构的版本；列表与正文必须同一个版本，否则正文要重新取。
  final int docVersion;
  final List<ArticleBlock> blocks;

  /// 源内容撤权后，列表仍可能出现占位，但正文不得再返回。
  final bool revoked;
}

class Publisher {
  const Publisher({
    required this.id,
    required this.name,
    required this.description,
    required this.unread,
    required this.subscribed,
    required this.version,
  });

  final String id;
  final String name;
  final String description;
  final int unread;
  final bool subscribed;

  /// 订阅关系的版本：关注/取关用 expected_version 防止两端互相覆盖。
  final int version;
}

class FavoriteItem {
  const FavoriteItem({
    required this.id,
    required this.type,
    required this.title,
    required this.snippet,
    required this.source,
    required this.savedAt,
    required this.available,
  });

  final String id;

  /// `article` / `moment` / `file` / `link` / `note`。
  final String type;
  final String title;
  final String snippet;
  final String source;
  final DateTime savedAt;

  /// 源内容撤权后为 false：条目仍可见，但不能再打开正文。
  final bool available;
}

class StickerPack {
  const StickerPack({
    required this.id,
    required this.name,
    required this.count,
    required this.cover,
    required this.downloaded,
  });

  final String id;
  final String name;
  final int count;
  final String cover;
  final bool downloaded;
}

// ---------------------------------------------------------------------------
// 视频 / 直播 / 通话
// ---------------------------------------------------------------------------

class VideoItem {
  const VideoItem({
    required this.id,
    required this.author,
    required this.title,
    required this.durationMs,
    required this.coverColor,
    required this.playable,
  });

  final String id;
  final String author;
  final String title;
  final int durationMs;
  final int coverColor;

  /// 转码未完成或已下架时为 false；这类条目不能显示成可播放。
  final bool playable;
}

class LiveSession {
  const LiveSession({
    required this.channel,
    required this.title,
    required this.live,
    required this.viewers,
    required this.hlsUrl,
    required this.startedAt,
  });

  final String channel;
  final String title;
  final bool live;
  final int viewers;
  final String hlsUrl;
  final DateTime startedAt;
}

enum CallPhase { idle, dialing, ringing, connecting, active, ended }

class CallSnapshot {
  const CallSnapshot({
    required this.callId,
    required this.convId,
    required this.peerName,
    required this.phase,
    required this.muted,
    required this.cameraOn,
    required this.speakerOn,
    required this.bluetoothOn,
    required this.frontCamera,
    required this.video,
    required this.minimized,
    required this.endedReason,
    required this.startedAt,
  });

  final String callId;
  final String convId;
  final String peerName;
  final CallPhase phase;
  final bool muted;
  final bool cameraOn;
  final bool speakerOn;
  final bool bluetoothOn;
  final bool frontCamera;
  final bool video;
  final bool minimized;

  /// 结束原因由服务端判定（对端挂断 / 超时 / 网络中断），界面不猜。
  final String endedReason;

  /// 通话建立时间；时长由本地按它推进，不自己从页面打开时开始算。
  final DateTime startedAt;

  bool get inProgress =>
      phase != CallPhase.idle &&
      phase != CallPhase.ended &&
      phase != CallPhase.connecting;

  CallSnapshot copyWith({
    CallPhase? phase,
    bool? muted,
    bool? cameraOn,
    bool? speakerOn,
    bool? bluetoothOn,
    bool? frontCamera,
    bool? minimized,
    String? endedReason,
  }) => CallSnapshot(
    callId: callId,
    convId: convId,
    peerName: peerName,
    phase: phase ?? this.phase,
    muted: muted ?? this.muted,
    cameraOn: cameraOn ?? this.cameraOn,
    speakerOn: speakerOn ?? this.speakerOn,
    bluetoothOn: bluetoothOn ?? this.bluetoothOn,
    frontCamera: frontCamera ?? this.frontCamera,
    video: video,
    minimized: minimized ?? this.minimized,
    endedReason: endedReason ?? this.endedReason,
    startedAt: startedAt,
  );
}

// ---------------------------------------------------------------------------
// 商业
// ---------------------------------------------------------------------------

class WalletAccount {
  const WalletAccount({
    required this.balance,
    required this.virtualBalance,
    required this.currency,
    required this.frozen,
  });

  final Money balance;

  /// 虚拟币与真钱必须分开展示，不能合成一个"余额"。
  final Money virtualBalance;
  final String currency;
  final bool frozen;
}

class BankCard {
  const BankCard({
    required this.id,
    required this.bank,
    required this.maskedNumber,
    required this.isDefault,
    required this.boundAt,
  });

  final String id;
  final String bank;
  final String maskedNumber;
  final bool isDefault;
  final DateTime boundAt;
}

class BillEntry {
  const BillEntry({
    required this.id,
    required this.title,
    required this.amount,
    required this.direction,
    required this.createdAt,
    required this.state,
  });

  final String id;
  final String title;
  final Money amount;

  /// `in` / `out`。
  final String direction;
  final DateTime createdAt;
  final String state;
}

class PaymentIntent {
  const PaymentIntent({
    required this.id,
    required this.payee,
    required this.amount,
    required this.state,
    required this.expiresAt,
    required this.confirmation,
  });

  final String id;
  final String payee;
  final Money amount;

  /// 只认服务端状态；`SUCCEEDED` 之外一律不能当到账。
  final String state;
  final DateTime expiresAt;

  /// 危险动作才有确认票据；为空表示不做二次确认。
  final ActionConfirmation? confirmation;

  bool get succeeded => state == 'SUCCEEDED';
}

class RedPacket {
  const RedPacket({
    required this.id,
    required this.total,
    required this.remaining,
    required this.count,
    required this.remainingCount,
    required this.claimed,
    required this.claimedAmount,
    required this.expiresAt,
  });

  final String id;
  final Money total;
  final Money remaining;
  final int count;
  final int remainingCount;
  final bool claimed;

  /// 自己抢到的金额；未抢为空。
  final Money? claimedAmount;
  final DateTime expiresAt;

  bool get exhausted => remainingCount <= 0 || remaining.isZero;
}

class CardEntry {
  const CardEntry({
    required this.id,
    required this.kind,
    required this.title,
    required this.subtitle,
    required this.validUntil,
    required this.usable,
  });

  /// `member` / `coupon` / `ticket`。
  final String id;
  final String kind;
  final String title;
  final String subtitle;
  final DateTime validUntil;
  final bool usable;
}

class TransferDraft {
  const TransferDraft({
    required this.convId,
    required this.payeeId,
    required this.payeeName,
    required this.amount,
    required this.note,
    required this.paymentMethodId,
  });

  final String convId;
  final String payeeId;
  final String payeeName;
  final Money amount;
  final String note;

  /// `balance` 或银行卡 id；服务端按它选择扣款渠道。
  final String paymentMethodId;
}

// ---------------------------------------------------------------------------
// 发现 / 扫码 / 识别 / 小程序
// ---------------------------------------------------------------------------

class ScanResult {
  const ScanResult({
    required this.kind,
    required this.raw,
    required this.targetId,
    required this.safe,
    required this.reason,
  });

  /// `url` / `payment` / `contact` / `mini` / `text`。
  final String kind;
  final String raw;

  /// 解析后的内部 id；界面只凭它路由，不自行执行 URL。
  final String targetId;

  /// 安全深链校验结果。false 时只能展示，不能跳转。
  final bool safe;
  final String reason;
}

class RecognitionResult {
  const RecognitionResult({
    required this.label,
    required this.confidence,
    required this.translation,
    required this.sourceText,
    required this.targetLanguage,
  });

  final String label;
  final double confidence;
  final String translation;
  final String sourceText;
  final String targetLanguage;
}

class NearbyPlace {
  const NearbyPlace({
    required this.id,
    required this.name,
    required this.distanceMeters,
    required this.cell,
  });

  final String id;
  final String name;
  final int distanceMeters;

  /// 粗粒度网格；精确坐标不默认暴露给界面。
  final String cell;
}

class ServiceItem {
  const ServiceItem({
    required this.id,
    required this.category,
    required this.title,
    required this.provider,
    required this.fromPrice,
  });

  final String id;
  final String category;
  final String title;
  final String provider;
  final Money? fromPrice;
}

class ServiceQuote {
  const ServiceQuote({
    required this.quoteId,
    required this.serviceId,
    required this.price,
    required this.expiresAt,
    required this.terms,
  });

  final String quoteId;
  final String serviceId;
  final Money price;
  final DateTime expiresAt;

  /// 退改条件等必须展示给用户的条款。
  final String terms;
}

class MiniAppInfo {
  const MiniAppInfo({
    required this.appId,
    required this.name,
    required this.version,
    required this.installed,
    required this.recent,
    required this.revoked,
    required this.scopes,
  });

  final String appId;
  final String name;
  final String version;
  final bool installed;
  final bool recent;

  /// 服务端撤销的版本不能再启动。
  final bool revoked;
  final List<String> scopes;
}

// ---------------------------------------------------------------------------
// 搜索 / 设置 / 资料
// ---------------------------------------------------------------------------

class SearchHit {
  const SearchHit({
    required this.type,
    required this.id,
    required this.title,
    required this.snippet,
    required this.source,
    required this.available,
  });

  final String type;
  final String id;
  final String title;
  final String snippet;
  final String source;

  /// 撤权/删除后为 false；结果里保留占位，但不给打开入口。
  final bool available;
}

class ProfileInfo {
  const ProfileInfo({
    required this.userId,
    required this.name,
    required this.signature,
    required this.region,
    required this.avatarMediaId,
    required this.version,
  });

  final String userId;
  final String name;
  final String signature;
  final String region;
  final String avatarMediaId;

  /// 资料版本：更新时作为 expected_version 回传，冲突由服务端拒绝。
  final int version;
}

class Preferences {
  const Preferences({
    required this.messageNotifications,
    required this.sound,
    required this.vibration,
    required this.findMeByPhone,
    required this.walletLock,
    required this.youthMode,
    required this.darkMode,
    required this.language,
    required this.version,
  });

  final bool messageNotifications;
  final bool sound;
  final bool vibration;
  final bool findMeByPhone;
  final bool walletLock;
  final bool youthMode;
  final bool darkMode;
  final String language;

  /// patch 版本：并发修改由服务端检测冲突。
  final int version;

  Preferences copyWith({
    bool? messageNotifications,
    bool? sound,
    bool? vibration,
    bool? findMeByPhone,
    bool? walletLock,
    bool? youthMode,
    bool? darkMode,
    String? language,
  }) => Preferences(
    messageNotifications: messageNotifications ?? this.messageNotifications,
    sound: sound ?? this.sound,
    vibration: vibration ?? this.vibration,
    findMeByPhone: findMeByPhone ?? this.findMeByPhone,
    walletLock: walletLock ?? this.walletLock,
    youthMode: youthMode ?? this.youthMode,
    darkMode: darkMode ?? this.darkMode,
    language: language ?? this.language,
    version: version,
  );
}

class StorageUsage {
  const StorageUsage({
    required this.cacheBytes,
    required this.mediaBytes,
    required this.databaseBytes,
  });

  final int cacheBytes;
  final int mediaBytes;
  final int databaseBytes;

  int get totalBytes => cacheBytes + mediaBytes + databaseBytes;
}

class SecurityInfo {
  const SecurityInfo({
    required this.phoneMasked,
    required this.passwordSet,
    required this.soundLock,
    required this.deviceCount,
    required this.lastVerifiedAt,
  });

  final String phoneMasked;
  final bool passwordSet;
  final bool soundLock;
  final int deviceCount;
  final DateTime lastVerifiedAt;
}

class BackupFile {
  const BackupFile({
    required this.fileId,
    required this.createdAt,
    required this.sizeBytes,
    required this.verified,
  });

  final String fileId;
  final DateTime createdAt;
  final int sizeBytes;
  final bool verified;
}

// ---------------------------------------------------------------------------
// 状态 / 位置 / 名片码
// ---------------------------------------------------------------------------

class UserStatus {
  const UserStatus({
    required this.id,
    required this.userId,
    required this.displayName,
    required this.text,
    required this.background,
    required this.expiresAt,
    required this.visibility,
  });

  final String id;
  final String userId;
  final String displayName;
  final String text;
  final String background;
  final DateTime expiresAt;

  /// `friends` / `close_friends` / `private`。
  final String visibility;

  bool expiredAt(DateTime now) => !expiresAt.isAfter(now);
}

class LocationShare {
  const LocationShare({
    required this.id,
    required this.convId,
    required this.placeName,
    required this.address,
    required this.latitude,
    required this.longitude,
    required this.active,
    required this.expiresAt,
  });

  final String id;
  final String convId;
  final String placeName;
  final String address;

  /// 精确坐标只在"正在共享"时由服务端下发；界面不缓存到分享结束之后。
  final double latitude;
  final double longitude;
  final bool active;
  final DateTime expiresAt;
}

enum QrStyle {
  spring('春日绿'),
  ink('墨绿'),
  warm('暖阳'),
  mist('雾蓝');

  const QrStyle(this.label);

  final String label;
}

class MyQrCode {
  const MyQrCode({
    required this.payload,
    required this.style,
    required this.version,
    required this.expiresAt,
    required this.scannable,
  });

  final String payload;
  final QrStyle style;
  final int version;
  final DateTime expiresAt;

  /// 服务端确认该码可被标准扫码器解码；false 时不能宣称"可扫"。
  final bool scannable;
}
