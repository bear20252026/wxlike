package com.wxlike.wxlike_mobile

import android.Manifest
import android.app.Activity
import android.app.NotificationManager
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.location.LocationManager
import android.media.MediaPlayer
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.provider.Settings
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import androidx.core.content.FileProvider
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * G12-05 的 Android 平台桥。
 *
 * 线协议（必须与 lib/platform/method_channel_platform.dart 逐条对齐）：
 * - `permission.status` / `permission.request` {permission} -> "granted|limited|denied|permanentlyDenied|restricted"
 * - `photo.pick` / `photo.capture` / `file.pick` -> {ref,kind,name,mime_type,bytes,duration_ms} 或 null（用户取消）
 * - `settings.open` {page: app|notifications|location}
 * - `location.current` -> {latitude,longitude,accuracy_m,captured_at_ms}
 * - `network.status` -> "online|offline"；事件通道 `wxlike/platform/network` 推同一字符串
 * - 事件通道 `wxlike/platform/links` 推深链字符串（onNewIntent / 冷启动 VIEW intent）
 * - `secret.read{key}` / `secret.write{key,value}` / `secret.delete{key}`
 * - `push.token` -> null（FCM 未配置）；`push.revoke` -> 报 UNAVAILABLE
 * - `copyText{text}`（额外提供；Dart 侧剪贴板目前走 flutter/services，不经此通道）
 *
 * 错误码口径：Dart 把 MissingPluginException（通道上没有处理器）映射成
 * PLATFORM_UNWIRED；Kotlin 侧"明确没接线"用 PlatformException("UNAVAILABLE")，
 * 两者在 Dart 端都归到 G12FailureKind.unavailable。
 *
 * 已知限制（按审计要求逐条注明）：
 * - `voice.start/stop`、`audio.route`、`gallery.save` 返回 UNAVAILABLE：
 *   录音需要 MediaRecorder 生命周期管理、扬声器切换需要通话音频模式、
 *   相册写入需要分版本处理 MediaStore 权限，风险高于本轮范围。
 * - `permission.status` 不返回 permanentlyDenied：Android 在"从未询问过"与
 *   "永久拒绝"时 shouldShowRequestPermissionRationale 都为 false，无法区分；
 *   只有 `permission.request` 在真实拒绝之后才给出 permanentlyDenied。
 * - `location.current` 只返回系统缓存的定位点（getLastKnownLocation），
 *   不发起新的定位请求。
 * - secret 存储用 AndroidKeyStore AES-GCM + 应用私有目录文件（小数据量，
 *   主线程读写）；未接 security-crypto 的 EncryptedSharedPreferences。
 * - 网络 onLost 在 Wi-Fi/蜂窝切换瞬间可能闪断一次"offline"。
 */
class MainActivity : FlutterActivity() {

    private val mainHandler = Handler(Looper.getMainLooper())

    /** activity result 的在途请求：requestCode -> 上下文。 */
    private class PendingPick(
        val result: MethodChannel.Result,
        val defaultKind: String,
        val persistable: Boolean,
        val captureFile: File? = null,
    )

    private val pending = HashMap<Int, PendingPick>()
    private var nextRequestCode = 47000
    private var permissionResult: MethodChannel.Result? = null

    private var linksSink: EventChannel.EventSink? = null
    private var pendingInitialLink: String? = null
    private var deliveredInitialLink = false

    private var networkSink: EventChannel.EventSink? = null
    private var connectivityManager: ConnectivityManager? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    private var secrets: KeystoreSecretStore? = null
    private var mediaPlayer: MediaPlayer? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        // 冷启动深链：configureFlutterEngine 之后事件通道才挂上，先存起来。
        if (intent?.action == Intent.ACTION_VIEW) {
            pendingInitialLink = intent.dataString
        }
        super.onCreate(savedInstanceState)
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        val messenger = flutterEngine.dartExecutor.binaryMessenger
        MethodChannel(messenger, "wxlike/platform").setMethodCallHandler(::onMethodCall)
        EventChannel(messenger, "wxlike/platform/network").setStreamHandler(networkStreamHandler)
        EventChannel(messenger, "wxlike/platform/links").setStreamHandler(linksStreamHandler)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (intent.action == Intent.ACTION_VIEW) {
            intent.dataString?.let { link ->
                mainHandler.post { linksSink?.success(link) }
            }
        }
    }

    private val linksStreamHandler = object : EventChannel.StreamHandler {
        override fun onListen(args: Any?, sink: EventChannel.EventSink) {
            linksSink = sink
            if (!deliveredInitialLink) {
                deliveredInitialLink = true
                pendingInitialLink?.let { sink.success(it) }
                pendingInitialLink = null
            }
        }

        override fun onCancel(args: Any?) {
            linksSink = null
        }
    }

    private val networkStreamHandler = object : EventChannel.StreamHandler {
        override fun onListen(args: Any?, sink: EventChannel.EventSink) {
            networkSink = sink
            val cm = getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
            connectivityManager = cm
            val callback = object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) = postNetworkStatus(true)

                override fun onLost(network: Network) = postNetworkStatus(false)
            }
            networkCallback = callback
            try {
                cm.registerNetworkCallback(
                    NetworkRequest.Builder()
                        .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                        .build(),
                    callback,
                )
                sink.success(if (isNetworkOnline(cm)) "online" else "offline")
            } catch (error: Exception) {
                sink.error("UNAVAILABLE", "注册网络回调失败: ${error.message}", null)
            }
        }

        override fun onCancel(args: Any?) {
            networkCallback?.let { callback ->
                try {
                    connectivityManager?.unregisterNetworkCallback(callback)
                } catch (_: Exception) {
                    // 注销失败只影响后续回调，通道已关，无需处理。
                }
            }
            networkCallback = null
            networkSink = null
        }
    }

    private fun postNetworkStatus(online: Boolean) {
        mainHandler.post { networkSink?.success(if (online) "online" else "offline") }
    }

    private fun isNetworkOnline(cm: ConnectivityManager? = null): Boolean {
        val manager = cm ?: getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return false
        val network = manager.activeNetwork ?: return false
        val caps = manager.getNetworkCapabilities(network) ?: return false
        return caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
    }

    // ------------------------------------------------------------------
    // MethodChannel 分发
    // ------------------------------------------------------------------

    private fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        try {
            when (call.method) {
                "permission.status" -> permissionStatus(call.argument<String>("permission"), result)
                "permission.request" -> permissionRequest(call.argument<String>("permission"), result)
                "settings.open" -> openSettings(call.argument<String>("page"), result)
                "network.status" -> result.success(if (isNetworkOnline()) "online" else "offline")

                "photo.pick" -> pickContent(
                    result,
                    Intent(Intent.ACTION_PICK, MediaStore.Images.Media.EXTERNAL_CONTENT_URI),
                    defaultKind = "image",
                    persistable = false,
                )
                "file.pick" -> pickContent(
                    result,
                    Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                        addCategory(Intent.CATEGORY_OPENABLE)
                        type = "*/*"
                    },
                    defaultKind = "",
                    persistable = true,
                )
                "photo.capture" -> capturePhoto(result)

                "voice.start", "voice.stop" -> unavailable(
                    result,
                    "voice.* 未接线：录音采集需要 MediaRecorder 生命周期管理（见类注释）",
                )
                "audio.route" -> unavailable(
                    result,
                    "扬声器切换未接线：需要 AudioManager.MODE_IN_COMMUNICATION（见类注释）",
                )
                "gallery.save" -> unavailable(
                    result,
                    "相册写入未接线：需要分版本处理 MediaStore 权限（见类注释）",
                )

                "audio.play" -> playAudio(call.argument<String>("ref"), result)
                "audio.stop" -> stopAudio(result)
                "location.current" -> currentLocation(result)

                "secret.read" -> secretCall(result) { it.read(call.argument<String>("key") ?: "") }
                "secret.write" -> secretCall(result) {
                    it.write(
                        call.argument<String>("key") ?: "",
                        call.argument<String>("value") ?: "",
                    )
                    null
                }
                "secret.delete" -> secretCall(result) {
                    it.delete(call.argument<String>("key") ?: "")
                    null
                }

                "push.token" -> result.success(null) // FCM/APNs 未配置：Dart 视 null 为"没有推送"
                "push.revoke" -> unavailable(result, "推送未配置（FCM/APNs 需宿主接入）")

                "copyText" -> copyText(call.argument<String>("text") ?: "", result)
                else -> result.notImplemented()
            }
        } catch (error: SecurityException) {
            result.error("PERMISSION_DENIED", error.message ?: "缺少系统权限", null)
        } catch (error: Exception) {
            result.error("UNAVAILABLE", "${error.javaClass.simpleName}: ${error.message}", null)
        }
    }

    /** Kotlin 侧"明确未接线"的统一出口（错误码口径见类注释）。 */
    private fun unavailable(result: MethodChannel.Result, detail: String) =
        result.error("UNAVAILABLE", detail, null)

    // ------------------------------------------------------------------
    // 权限
    // ------------------------------------------------------------------

    private fun permissionsFor(name: String?): List<String> = when (name) {
        "camera" -> listOf(Manifest.permission.CAMERA)
        "microphone" -> listOf(Manifest.permission.RECORD_AUDIO)
        "photos", "files" ->
            if (Build.VERSION.SDK_INT >= 33) {
                listOf(
                    Manifest.permission.READ_MEDIA_IMAGES,
                    Manifest.permission.READ_MEDIA_VIDEO,
                )
            } else {
                listOf(Manifest.permission.READ_EXTERNAL_STORAGE)
            }
        "location" -> listOf(
            Manifest.permission.ACCESS_FINE_LOCATION,
            Manifest.permission.ACCESS_COARSE_LOCATION,
        )
        "notifications" ->
            if (Build.VERSION.SDK_INT >= 33) {
                listOf(Manifest.permission.POST_NOTIFICATIONS)
            } else {
                emptyList() // 13 以下没有运行时通知权限，跟随系统开关
            }
        else -> emptyList()
    }

    private fun isGranted(permission: String) =
        checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED

    private fun wireStatus(perms: List<String>): String {
        if (perms.all(::isGranted)) return "granted"
        // Android 14"部分照片访问"：完整媒体权限没给、但用户选了部分照片。
        if (Build.VERSION.SDK_INT >= 34 &&
            perms.any { it == Manifest.permission.READ_MEDIA_IMAGES || it == Manifest.permission.READ_MEDIA_VIDEO } &&
            isGranted(Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED)
        ) {
            return "limited"
        }
        // status 查询不区分 permanentlyDenied：从未询问与永久拒绝在系统 API
        // 里无法区分（见类注释），统一报 denied，由 request 之后再判定。
        return "denied"
    }

    private fun permissionStatus(name: String?, result: MethodChannel.Result) {
        val perms = permissionsFor(name)
        if (perms.isEmpty()) {
            if (name == "notifications" && Build.VERSION.SDK_INT < 33) {
                val enabled = getSystemService(NotificationManager::class.java)
                    ?.areNotificationsEnabled() == true
                result.success(if (enabled) "granted" else "denied")
            } else {
                result.error("UNAVAILABLE", "未知权限: $name", null)
            }
            return
        }
        result.success(wireStatus(perms))
    }

    private fun permissionRequest(name: String?, result: MethodChannel.Result) {
        val perms = permissionsFor(name)
        if (perms.isEmpty()) {
            if (name == "notifications" && Build.VERSION.SDK_INT < 33) {
                // 13 以下无法运行时申请：直接打开系统通知设置由用户操作。
                val enabled = getSystemService(NotificationManager::class.java)
                    ?.areNotificationsEnabled() == true
                if (enabled) {
                    result.success("granted")
                } else {
                    openNotificationSettings()
                    result.success("denied")
                }
            } else {
                result.error("UNAVAILABLE", "未知权限: $name", null)
            }
            return
        }
        if (perms.all(::isGranted)) {
            result.success("granted")
            return
        }
        if (permissionResult != null) {
            // 一次只挂一个权限请求；并发的按当前状态返回。
            result.success(wireStatus(perms))
            return
        }
        permissionResult = result
        requestPermissions(perms.toTypedArray(), REQUEST_PERMISSIONS)
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != REQUEST_PERMISSIONS) return
        val result = permissionResult ?: return
        permissionResult = null
        val denied = permissions
            .filterIndexed { index, _ -> grantResults.getOrNull(index) != PackageManager.PERMISSION_GRANTED }
        val wire = when {
            denied.isEmpty() -> "granted"
            Build.VERSION.SDK_INT >= 34 &&
                denied.any {
                    it == Manifest.permission.READ_MEDIA_IMAGES || it == Manifest.permission.READ_MEDIA_VIDEO
                } &&
                isGranted(Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED) -> "limited"
            // 已知偏差：第一次拒绝时 rationale 仍可能为 false，会被判成
            // permanentlyDenied；与 permission_handler 同口径，接受该近似。
            denied.any { !shouldShowRequestPermissionRationale(it) } -> "permanentlyDenied"
            else -> "denied"
        }
        result.success(wire)
    }

    // ------------------------------------------------------------------
    // 系统设置 / 剪贴板
    // ------------------------------------------------------------------

    private fun openSettings(page: String?, result: MethodChannel.Result) {
        val intent = when (page) {
            "notifications" -> openNotificationSettings()
            "location" -> Intent(Settings.ACTION_LOCATION_SOURCE_SETTINGS)
            else -> Intent(
                Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                Uri.fromParts("package", packageName, null),
            )
        }
        try {
            startActivity(intent)
            result.success(null)
        } catch (error: ActivityNotFoundException) {
            result.error("UNAVAILABLE", "打不开系统设置: ${error.message}", null)
        }
    }

    private fun openNotificationSettings(): Intent =
        Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, packageName)

    private fun copyText(text: String, result: MethodChannel.Result) {
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("wxlike", text))
        result.success(null)
    }

    // ------------------------------------------------------------------
    // 媒体选择 / 拍照
    // ------------------------------------------------------------------

    private fun pickContent(
        result: MethodChannel.Result,
        intent: Intent,
        defaultKind: String,
        persistable: Boolean,
    ) {
        val code = nextRequestCode++
        pending[code] = PendingPick(result, defaultKind, persistable)
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(intent, code)
        } catch (error: ActivityNotFoundException) {
            pending.remove(code)
            result.error("UNAVAILABLE", "没有可用的选择器: ${error.message}", null)
        }
    }

    private fun capturePhoto(result: MethodChannel.Result) {
        // manifest 里声明了 CAMERA，所以发起 ACTION_IMAGE_CAPTURE 前必须真的持有它。
        if (!isGranted(Manifest.permission.CAMERA)) {
            result.error(
                if (shouldShowRequestPermissionRationale(Manifest.permission.CAMERA)) {
                    "PERMISSION_DENIED"
                } else {
                    "PERMISSION_PERMANENT"
                },
                "需要相机权限（请先通过 permission.request 申请）",
                null,
            )
            return
        }
        val dir = File(filesDir, "captures").apply { mkdirs() }
        val file = File(dir, "capture_${System.currentTimeMillis()}.jpg")
        val uri = FileProvider.getUriForFile(this, "$packageName.fileprovider", file)
        val intent = Intent(MediaStore.ACTION_IMAGE_CAPTURE).apply {
            putExtra(MediaStore.EXTRA_OUTPUT, uri)
            // FLAG_GRANT Write 已通过 EXTRA_OUTPUT 授权；clipData 兼容部分相机应用。
            clipData = ClipData.newRawUri("output", uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        }
        val code = nextRequestCode++
        pending[code] = PendingPick(result, defaultKind = "image", persistable = false, captureFile = file)
        try {
            @Suppress("DEPRECATION")
            startActivityForResult(intent, code)
        } catch (error: ActivityNotFoundException) {
            pending.remove(code)
            result.error("UNAVAILABLE", "没有可用的相机应用: ${error.message}", null)
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        val pick = pending.remove(requestCode) ?: return
        val captureFile = pick.captureFile
        if (captureFile != null) {
            if (resultCode == Activity.RESULT_OK && captureFile.exists()) {
                pick.result.success(
                    mapOf<String, Any>(
                        "ref" to captureFile.absolutePath,
                        "kind" to "image",
                        "name" to captureFile.name,
                        "mime_type" to "image/jpeg",
                        "bytes" to captureFile.length(),
                        "duration_ms" to 0L,
                    ),
                )
            } else {
                captureFile.delete()
                pick.result.success(null) // 用户取消
            }
            return
        }
        if (resultCode != Activity.RESULT_OK || data?.data == null) {
            pick.result.success(null) // 用户取消
            return
        }
        val uri = data.data!!
        if (pick.persistable) {
            // SAF 的持久授权不是每台设备都给；拿不到也不影响本次使用。
            try {
                contentResolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            } catch (_: SecurityException) {
                // 忽略：uri 可能过期，届时 Dart 侧读 ref 会拿到明确错误。
            }
        }
        val mime = contentResolver.getType(uri) ?: ""
        pick.result.success(
            mapOf<String, Any>(
                "ref" to uri.toString(),
                "kind" to kindForMime(mime, pick.defaultKind),
                "name" to queryColumn(uri, OpenableColumns.DISPLAY_NAME),
                "mime_type" to mime,
                "bytes" to (queryColumn(uri, OpenableColumns.SIZE).toLongOrNull() ?: 0L),
                "duration_ms" to 0L,
            ),
        )
    }

    private fun kindForMime(mime: String, fallback: String): String = when {
        mime.startsWith("image/") -> "image"
        mime.startsWith("video/") -> "video"
        mime.startsWith("audio/") -> "audio"
        else -> fallback.ifEmpty { "file" }
    }

    private fun queryColumn(uri: Uri, column: String): String {
        contentResolver.query(uri, arrayOf(column), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val index = cursor.getColumnIndex(column)
                if (index >= 0) return cursor.getString(index) ?: ""
            }
        }
        return uri.lastPathSegment ?: ""
    }

    // ------------------------------------------------------------------
    // 音频 / 定位
    // ------------------------------------------------------------------

    private fun playAudio(ref: String?, result: MethodChannel.Result) {
        if (ref.isNullOrEmpty()) {
            result.error("UNAVAILABLE", "audio.play 需要 ref", null)
            return
        }
        releasePlayer()
        try {
            val player = MediaPlayer()
            player.setDataSource(this, Uri.parse(ref))
            player.setOnPreparedListener { it.start() }
            player.setOnCompletionListener {
                it.release()
                if (mediaPlayer === it) mediaPlayer = null
            }
            player.setOnErrorListener { failing, _, _ ->
                failing.release()
                if (mediaPlayer === failing) mediaPlayer = null
                true
            }
            player.prepareAsync()
            mediaPlayer = player
            result.success(null)
        } catch (error: Exception) {
            result.error("UNAVAILABLE", "无法播放 $ref: ${error.message}", null)
        }
    }

    private fun stopAudio(result: MethodChannel.Result) {
        releasePlayer()
        result.success(null)
    }

    private fun releasePlayer() {
        mediaPlayer?.let { player ->
            try {
                if (player.isPlaying) player.stop()
            } catch (_: IllegalStateException) {
                // 未初始化就 stop 会抛；直接释放即可。
            }
            player.release()
        }
        mediaPlayer = null
    }

    private fun currentLocation(result: MethodChannel.Result) {
        val hasFine = isGranted(Manifest.permission.ACCESS_FINE_LOCATION)
        val hasCoarse = isGranted(Manifest.permission.ACCESS_COARSE_LOCATION)
        if (!hasFine && !hasCoarse) {
            result.error(
                if (shouldShowRequestPermissionRationale(Manifest.permission.ACCESS_FINE_LOCATION)) {
                    "PERMISSION_DENIED"
                } else {
                    "PERMISSION_PERMANENT"
                },
                "没有定位权限",
                null,
            )
            return
        }
        val manager = getSystemService(Context.LOCATION_SERVICE) as LocationManager
        val latest = listOf(
            LocationManager.GPS_PROVIDER,
            LocationManager.NETWORK_PROVIDER,
            LocationManager.PASSIVE_PROVIDER,
        )
            .filter { provider ->
                try {
                    manager.isProviderEnabled(provider)
                } catch (_: SecurityException) {
                    false
                }
            }
            .mapNotNull { provider ->
                try {
                    manager.getLastKnownLocation(provider)
                } catch (_: SecurityException) {
                    null
                }
            }
            .maxByOrNull { it.time }
        if (latest == null) {
            result.error("TIMEOUT", "拿不到定位（设备没有可用的历史定位点）", null)
            return
        }
        result.success(
            mapOf<String, Any>(
                "latitude" to latest.latitude,
                "longitude" to latest.longitude,
                "accuracy_m" to latest.accuracy.toDouble(),
                "captured_at_ms" to latest.time,
            ),
        )
    }

    // ------------------------------------------------------------------
    // 安全存储
    // ------------------------------------------------------------------

    private fun secretStore(): KeystoreSecretStore =
        secrets ?: KeystoreSecretStore(applicationContext).also { secrets = it }

    private inline fun secretCall(
        result: MethodChannel.Result,
        block: (KeystoreSecretStore) -> Any?,
    ) {
        try {
            val value = block(secretStore())
            result.success(value)
        } catch (error: Exception) {
            result.error("UNAVAILABLE", "安全存储读写失败: ${error.message}", null)
        }
    }

    override fun onDestroy() {
        networkCallback?.let { callback ->
            try {
                connectivityManager?.unregisterNetworkCallback(callback)
            } catch (_: Exception) {
                // 同 networkStreamHandler.onCancel。
            }
        }
        releasePlayer()
        pending.values.forEach { it.result.error("UNAVAILABLE", "Activity 已销毁", null) }
        pending.clear()
        super.onDestroy()
    }

    private companion object {
        // 固定权限请求码，避开 activity result 用的 47000+ 递增区间。
        const val REQUEST_PERMISSIONS = 46900
    }
}

/**
 * AndroidKeyStore AES-GCM + 应用私有目录文件的安全存储。
 *
 * 为什么不用 EncryptedSharedPreferences：那需要 androidx.security:security-crypto
 * 依赖与它的密钥迁移链；这里只存 PIN / 挑战 id 这类小值，keystore 内持钥、
 * 私有目录密文落盘已满足"系统级隔离"的要求，且没有额外构建期依赖。
 *
 * 局限：GCM 认证失败（密钥被换/文件损坏）会在 read 上抛异常，由调用方转成
 * UNAVAILABLE——绝不返回伪造的明文。
 */
internal class KeystoreSecretStore(context: Context) {
    private val dir = File(context.filesDir, "secrets").apply { mkdirs() }
    private val keyAlias = "wxlike_secret_key"

    private fun fileFor(key: String): File {
        // 文件名用 key 的 SHA-256：任意 key 字符串不能变成路径穿越。
        val digest = MessageDigest.getInstance("SHA-256").digest(key.toByteArray(Charsets.UTF_8))
        return File(dir, digest.joinToString("") { "%02x".format(it) })
    }

    @Synchronized
    fun read(key: String): String? {
        val file = fileFor(key)
        if (!file.exists()) return null
        val blob = file.readBytes()
        if (blob.size <= IV_LENGTH) return null
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, blob.copyOfRange(0, IV_LENGTH)))
        return String(cipher.doFinal(blob.copyOfRange(IV_LENGTH, blob.size)), Charsets.UTF_8)
    }

    @Synchronized
    fun write(key: String, value: String) {
        val file = fileFor(key)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val cipherText = cipher.doFinal(value.toByteArray(Charsets.UTF_8))
        val tmp = File("${file.absolutePath}.tmp")
        tmp.writeBytes(cipher.iv + cipherText)
        if (!tmp.renameTo(file)) {
            file.delete()
            check(tmp.renameTo(file)) { "写入安全存储失败: $key" }
        }
    }

    @Synchronized
    fun delete(key: String) {
        fileFor(key).delete()
    }

    private fun key(): SecretKey {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (keyStore.getKey(keyAlias, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                keyAlias,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(KEY_BITS)
                .build(),
        )
        return generator.generateKey()
    }

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_LENGTH = 12
        const val TAG_BITS = 128
        const val KEY_BITS = 256
    }
}
