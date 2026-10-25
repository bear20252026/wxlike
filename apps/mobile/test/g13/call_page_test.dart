import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:wxlike_mobile/features/calls/call_page.dart';
import 'package:wxlike_mobile/features/content/g13/models.dart';
import 'package:wxlike_mobile/features/content/g13/unavailable_ports.dart';

import 'g13_support.dart';

class _FakeCall extends UnavailableCallPort {
  int hangUps = 0;
  int releases = 0;
  int minimizes = 0;
  int mutes = 0;
  String? lastHangUpKey;
  Completer<void>? gate;

  CallSnapshot snapshot = CallSnapshot(
    callId: 'call-1',
    convId: 'c-1',
    peerName: 'Bob',
    phase: CallPhase.active,
    muted: false,
    cameraOn: true,
    speakerOn: false,
    bluetoothOn: false,
    frontCamera: true,
    video: false,
    minimized: false,
    endedReason: '',
    startedAt: DateTime.now().subtract(const Duration(minutes: 1)),
  );

  @override
  Stream<CallSnapshot> updates() => const Stream<CallSnapshot>.empty();

  @override
  Future<CallSnapshot> hangUp(
    String callId, {
    required String idempotencyKey,
  }) async {
    hangUps++;
    lastHangUpKey = idempotencyKey;
    if (gate != null) await gate!.future;
    snapshot = snapshot.copyWith(phase: CallPhase.ended, endedReason: 'peer');
    return snapshot;
  }

  @override
  Future<void> releaseMedia() async {
    releases++;
  }

  @override
  Future<CallSnapshot> setMinimized(String callId, bool minimized) async {
    minimizes++;
    snapshot = snapshot.copyWith(minimized: minimized);
    return snapshot;
  }

  @override
  Future<CallSnapshot> setMuted(
    String callId,
    bool muted, {
    required String idempotencyKey,
  }) async {
    mutes++;
    snapshot = snapshot.copyWith(muted: muted);
    return snapshot;
  }
}

Future<void> _open(WidgetTester tester, _FakeCall fake) async {
  await tester.pumpWidget(
    MaterialApp(
      home: CallPage(
        ports: testPorts(calls: fake),
        snapshot: fake.snapshot,
      ),
    ),
  );
  await tester.pump();
}

void main() {
  testWidgets('挂断：幂等键固定，随后释放媒体', (tester) async {
    final fake = _FakeCall();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.call_end));
    await tester.pump();
    await tester.pump();

    expect(fake.hangUps, 1);
    expect(fake.lastHangUpKey, 'call-hangup-call-1');
    expect(fake.releases, 1);
    expect(find.text('通话结束：peer'), findsOneWidget);
  });

  testWidgets('防重：挂断未返回时重复点击只发一次', (tester) async {
    final gate = Completer<void>();
    final fake = _FakeCall()..gate = gate;
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.call_end));
    await tester.pump();
    await tester.tap(find.byIcon(Icons.call_end), warnIfMissed: false);
    await tester.pump();

    expect(fake.hangUps, 1);
    gate.complete();
    await tester.pump();
    await tester.pump();
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('静音：状态由服务端返回值驱动', (tester) async {
    final fake = _FakeCall();
    await _open(tester, fake);

    await tester.tap(find.byIcon(Icons.mic));
    await tester.pump();
    await tester.pump();

    expect(fake.mutes, 1);
    expect(find.byIcon(Icons.mic_off), findsOneWidget);
    expect(find.text('已静音'), findsOneWidget);

    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('返回只最小化，不释放媒体', (tester) async {
    final fake = _FakeCall();
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: Center(
              child: TextButton(
                onPressed: () => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => CallPage(
                      ports: testPorts(calls: fake),
                      snapshot: fake.snapshot,
                    ),
                  ),
                ),
                child: const Text('open'),
              ),
            ),
          ),
        ),
      ),
    );
    await tester.tap(find.text('open'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    await tester.binding.handlePopRoute();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));

    expect(fake.minimizes, 1);
    expect(fake.releases, 0);
    expect(find.text('open'), findsOneWidget);
  });
}
