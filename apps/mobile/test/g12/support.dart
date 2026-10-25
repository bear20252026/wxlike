import 'dart:async';

import 'package:wxlike_mobile/platform/platform_ports.dart';

class MemorySecrets implements SecretStore {
  final Map<String, String> values = {};

  @override
  Future<String?> read(String key) async => values[key];

  @override
  Future<void> write(String key, String value) async {
    values[key] = value;
  }

  @override
  Future<void> delete(String key) async {
    values.remove(key);
  }
}

class FakeLifecycle implements LifecyclePort {
  final _controller = StreamController<AppLifecycleSignal>.broadcast();
  AppLifecycleSignal _current = AppLifecycleSignal.resumed;

  void emit(AppLifecycleSignal signal) {
    _current = signal;
    _controller.add(signal);
  }

  @override
  AppLifecycleSignal get current => _current;

  @override
  Stream<AppLifecycleSignal> get signals => _controller.stream;

  void close() => _controller.close();
}
