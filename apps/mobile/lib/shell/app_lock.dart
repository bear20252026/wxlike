/// 应用锁：PIN 存在系统安全存储里，切后台超过宽限期后重新锁定。
///
/// 这层只决定"界面能不能看"，不持有也不校验服务端 token——token 的权威在
/// Rust。冷启动时若设备已设置 PIN，直接锁定；恢复账号不等于跳过锁。
library;

import 'package:flutter/foundation.dart';

import '../platform/platform_ports.dart';
import 'contracts.dart';

class AppLockController extends ChangeNotifier {
  AppLockController({
    required this.secrets,
    this.grace = const Duration(seconds: 60),
    DateTime Function()? now,
  }) : _now = now ?? (() => DateTime.now().toUtc());

  static const String pinKey = 'g12.lock.pin';

  final SecretStore secrets;
  final Duration grace;
  final DateTime Function() _now;

  bool _enabled = false;
  bool _locked = false;
  bool _busy = false;
  String? _error;
  DateTime? _backgroundAt;

  bool get enabled => _enabled;
  bool get locked => _locked;
  bool get busy => _busy;
  String? get error => _error;

  static bool isValidPin(String value) => RegExp(r'^\d{4,6}$').hasMatch(value);

  Future<void> restore() async {
    _busy = true;
    _error = null;
    notifyListeners();
    try {
      final pin = await secrets.read(pinKey);
      _enabled = pin != null && pin.isNotEmpty;
      // 冷启动默认上锁：恢复账号不能绕过设备锁。
      _locked = _enabled;
    } catch (error) {
      final failure = asG12Failure(error, operation: '读取应用锁');
      if (failure.kind == G12FailureKind.unavailable) {
        // 平台安全存储尚未接线（PLATFORM_UNWIRED / UNAVAILABLE）：此时
        // unlock() 也读不了同一个存储，"启用并上锁"等于把用户锁死在一个
        // 永远解不开的壳里。宁可停用应用锁并给出可见警告。
        _enabled = false;
        _locked = false;
        _error = '设备安全存储不可用（${failure.code}），应用锁已停用';
      } else {
        // 其他失败（IO 抖动等）不能假装"没设锁"：保留锁定并提示重试。
        _error = '无法读取设备安全存储，暂时无法校验应用锁';
        _enabled = true;
        _locked = true;
      }
    } finally {
      _busy = false;
      notifyListeners();
    }
  }

  Future<bool> setPin(String pin) async {
    if (!isValidPin(pin)) {
      _error = 'PIN 需要 4–6 位数字';
      notifyListeners();
      return false;
    }
    _busy = true;
    _error = null;
    notifyListeners();
    try {
      await secrets.write(pinKey, pin);
      _enabled = true;
      _locked = false;
      return true;
    } catch (_) {
      _error = '写入设备安全存储失败';
      return false;
    } finally {
      _busy = false;
      notifyListeners();
    }
  }

  Future<bool> changePin(String current, String next) async {
    if (!await unlock(current)) return false;
    return setPin(next);
  }

  Future<bool> disable(String pin) async {
    if (!await unlock(pin)) return false;
    _busy = true;
    notifyListeners();
    try {
      await secrets.delete(pinKey);
      _enabled = false;
      _locked = false;
      return true;
    } catch (_) {
      _error = '清除设备安全存储失败';
      return false;
    } finally {
      _busy = false;
      notifyListeners();
    }
  }

  Future<bool> unlock(String pin) async {
    if (!_enabled) {
      _locked = false;
      notifyListeners();
      return true;
    }
    _busy = true;
    _error = null;
    notifyListeners();
    try {
      final stored = await secrets.read(pinKey);
      final ok = stored != null && _constantTimeEquals(stored, pin);
      if (ok) {
        _locked = false;
        _backgroundAt = null;
      } else {
        _error = 'PIN 不正确';
      }
      return ok;
    } catch (_) {
      _error = '无法读取设备安全存储';
      return false;
    } finally {
      _busy = false;
      notifyListeners();
    }
  }

  void lockNow() {
    if (!_enabled || _locked) return;
    _locked = true;
    notifyListeners();
  }

  void onBackground() {
    if (!_enabled) return;
    _backgroundAt = _now();
  }

  void onForeground() {
    if (!_enabled || _locked) return;
    final since = _backgroundAt;
    _backgroundAt = null;
    if (since != null && _now().difference(since) >= grace) {
      _locked = true;
      notifyListeners();
    }
  }

  static bool _constantTimeEquals(String a, String b) {
    if (a.length != b.length) return false;
    var diff = 0;
    for (var i = 0; i < a.length; i++) {
      diff |= a.codeUnitAt(i) ^ b.codeUnitAt(i);
    }
    return diff == 0;
  }
}
