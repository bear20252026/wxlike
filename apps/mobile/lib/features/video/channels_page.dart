/// G13-02：视频号。
///
/// 播放器同一时刻只保留一个：进入新视频前释放旧的，离页释放解码器与音频
/// 焦点。播放表面由平台桥提供（G12-05），这里只负责生命周期与状态，
/// 没有桥时明确显示未接线，不画一个假播放器冒充能播。
library;

import 'dart:async';

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

typedef MediaSurfaceBuilder = Widget Function(
  BuildContext context,
  String streamUrl,
);

class ChannelsPage extends StatefulWidget {
  const ChannelsPage({super.key, required this.ports, this.playerBuilder});

  final G13Ports ports;
  final MediaSurfaceBuilder? playerBuilder;

  @override
  State<ChannelsPage> createState() => _ChannelsPageState();
}

class _ChannelsPageState extends State<ChannelsPage> {
  late Future<PageResult<VideoItem>> _future = widget.ports.video.feed();

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.video.feed();
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '视频号',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: AsyncView<PageResult<VideoItem>>(
        future: _future,
        onRetry: _reload,
        isEmpty: (page) => page.isEmpty,
        emptyText: '还没有视频',
        builder: (context, page) => GridView.builder(
          padding: const EdgeInsets.all(12),
          gridDelegate: const SliverGridDelegateWithFixedCrossAxisCount(
            crossAxisCount: 2,
            mainAxisSpacing: 12,
            crossAxisSpacing: 12,
            childAspectRatio: 0.78,
          ),
          itemCount: page.items.length,
          itemBuilder: (context, index) {
            final video = page.items[index];
            return InkWell(
              onTap: video.playable
                  ? () => Navigator.of(context).push(
                      MaterialPageRoute<void>(
                        builder: (_) => ChannelPlayerPage(
                          ports: widget.ports,
                          video: video,
                          playerBuilder: widget.playerBuilder,
                        ),
                      ),
                    )
                  : null,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Expanded(
                    child: Container(
                      decoration: BoxDecoration(
                        color: Color(video.coverColor),
                        borderRadius: BorderRadius.circular(10),
                      ),
                      alignment: Alignment.center,
                      child: Icon(
                        video.playable
                            ? Icons.play_circle_outline
                            : Icons.hourglass_empty,
                        size: 36,
                        color: Colors.white70,
                      ),
                    ),
                  ),
                  const SizedBox(height: 6),
                  Text(
                    video.title,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                  ),
                  Text(
                    video.playable
                        ? '${video.author} · ${(video.durationMs / 1000).round()}s'
                        : '${video.author} · 转码中/已下架',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                ],
              ),
            );
          },
        ),
      ),
    );
  }
}

class ChannelPlayerPage extends StatefulWidget {
  const ChannelPlayerPage({
    super.key,
    required this.ports,
    required this.video,
    this.playerBuilder,
  });

  final G13Ports ports;
  final VideoItem video;
  final MediaSurfaceBuilder? playerBuilder;

  @override
  State<ChannelPlayerPage> createState() => _ChannelPlayerPageState();
}

class _ChannelPlayerPageState extends State<ChannelPlayerPage> {
  late Future<String> _url = widget.ports.video.playUrl(widget.video.id);

  @override
  void dispose() {
    // 离页必须释放，否则音频焦点与解码器会留在后台；释放失败也不能把
    // 异常抛进 dispose。
    unawaited(
      widget.ports.video.stopPlayback().then<void>(
        (_) {},
        onError: (Object _) {},
      ),
    );
    super.dispose();
  }

  void _reload() {
    if (!mounted) return;
    setState(() {
      _url = widget.ports.video.playUrl(widget.video.id);
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: widget.video.title,
      subtitle: widget.video.author,
      onBack: () => Navigator.of(context).maybePop(),
      body: AsyncView<String>(
        future: _url,
        onRetry: _reload,
        builder: (context, url) => Column(
          children: [
            AspectRatio(
              aspectRatio: 16 / 9,
              child: widget.playerBuilder != null
                  ? widget.playerBuilder!(context, url)
                  : Container(
                      color: Colors.black87,
                      alignment: Alignment.center,
                      child: const Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(
                            Icons.videocam_off_outlined,
                            color: Colors.white54,
                            size: 36,
                          ),
                          SizedBox(height: 8),
                          Text(
                            '播放器未接入\n需要 G12-05 平台桥',
                            textAlign: TextAlign.center,
                            style: TextStyle(
                              color: Colors.white54,
                              fontSize: 12,
                            ),
                          ),
                        ],
                      ),
                    ),
            ),
            ListTile(
              title: Text(widget.video.title),
              subtitle: Text(
                '${widget.video.author} · ${(widget.video.durationMs / 1000).round()}s',
              ),
            ),
          ],
        ),
      ),
    );
  }
}
