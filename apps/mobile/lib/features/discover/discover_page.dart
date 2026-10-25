/// G13-04：发现页与全入口。
///
/// "发现"里每个格子都指向一个真实页面；没有实现的入口不摆出来。点击后能看到的
/// 数据一律来自对应域端口，绝不用外链首页或静态卡片冒充内置页面。
library;

import 'package:flutter/material.dart';

import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import '../content/favorites_page.dart';
import '../content/moments_page.dart';
import '../content/official/official_page.dart';
import '../search/search_page.dart';
import '../video/channels_page.dart';
import '../video/live_page.dart';
import '../mini/mini_programs_page.dart';
import 'nearby_page.dart';
import 'recognition_page.dart';
import 'scan_page.dart';
import 'service_catalog_page.dart';

class DiscoverPage extends StatelessWidget {
  const DiscoverPage({super.key, required this.ports});

  final G13Ports ports;

  void _open(BuildContext context, Widget page) {
    Navigator.of(context).push(MaterialPageRoute<void>(builder: (_) => page));
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '发现',
      body: ListView(
        children: [
          _Entry(
            icon: Icons.camera_alt_outlined,
            title: '朋友圈',
            color: const Color(0xff4c8edb),
            onTap: () => _open(context, MomentsPage(ports: ports)),
          ),
          _Entry(
            icon: Icons.play_circle_outline,
            title: '视频号',
            color: const Color(0xffe8a431),
            onTap: () => _open(context, ChannelsPage(ports: ports)),
          ),
          _Entry(
            icon: Icons.live_tv_outlined,
            title: '直播',
            color: const Color(0xffd8472f),
            onTap: () =>
                _open(context, LivePage(ports: ports, channel: 'discover')),
          ),
          const Divider(height: 1),
          _Entry(
            icon: Icons.qr_code_scanner,
            title: '扫一扫',
            color: const Color(0xff3b7cd4),
            onTap: () => _open(context, ScanPage(ports: ports)),
          ),
          _Entry(
            icon: Icons.phone_android,
            title: '摇一摇',
            color: const Color(0xff765fc7),
            onTap: () => _open(context, NearbyPage(ports: ports, shake: true)),
          ),
          _Entry(
            icon: Icons.location_on_outlined,
            title: '附近',
            color: const Color(0xff07b25a),
            onTap: () => _open(context, NearbyPage(ports: ports)),
          ),
          _Entry(
            icon: Icons.search,
            title: '搜一搜',
            color: const Color(0xff67a3e8),
            onTap: () => _open(context, SearchPage(ports: ports)),
          ),
          _Entry(
            icon: Icons.translate,
            title: '识物与翻译',
            color: const Color(0xff8d7cdf),
            onTap: () => _open(context, RecognitionPage(ports: ports)),
          ),
          const Divider(height: 1),
          _Entry(
            icon: Icons.shopping_bag_outlined,
            title: '购物',
            color: const Color(0xffe8a431),
            onTap: () => _open(
              context,
              ServiceCatalogPage(ports: ports, category: 'shopping'),
            ),
          ),
          _Entry(
            icon: Icons.sports_esports_outlined,
            title: '游戏',
            color: const Color(0xff4c8edb),
            onTap: () => _open(
              context,
              ServiceCatalogPage(ports: ports, category: 'game'),
            ),
          ),
          _Entry(
            icon: Icons.widgets_outlined,
            title: '小程序',
            color: const Color(0xff07b25a),
            onTap: () => _open(context, MiniProgramsPage(ports: ports)),
          ),
          _Entry(
            icon: Icons.article_outlined,
            title: '公众号',
            color: const Color(0xff8d7cdf),
            onTap: () => _open(context, OfficialPage(ports: ports)),
          ),
          _Entry(
            icon: Icons.star_border,
            title: '收藏',
            color: const Color(0xff8b8b8b),
            onTap: () => openFavorites(context, ports),
          ),
          const SizedBox(height: 24),
        ],
      ),
    );
  }
}

class _Entry extends StatelessWidget {
  const _Entry({
    required this.icon,
    required this.title,
    required this.color,
    required this.onTap,
  });

  final IconData icon;
  final String title;
  final Color color;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: CircleAvatar(
        backgroundColor: color.withValues(alpha: 0.14),
        foregroundColor: color,
        child: Icon(icon),
      ),
      title: Text(title),
      trailing: const Icon(Icons.chevron_right),
      onTap: onTap,
    );
  }
}
