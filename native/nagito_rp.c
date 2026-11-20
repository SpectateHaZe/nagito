// nagito Remote Play shim: chiaki-ng session + ffmpeg decode + Opus audio + SDL2 DualSense input.
#define SDL_MAIN_HANDLED
#include "nagito_rp.h"

#include <chiaki/audio.h>
#include <chiaki/common.h>
#include <chiaki/discoveryservice.h>
#include <chiaki/ffmpegdecoder.h>
#include <chiaki/log.h>
#include <chiaki/opusdecoder.h>
#include <chiaki/opusencoder.h>
#include <chiaki/orientation.h>
#include <chiaki/regist.h>
#include <chiaki/session.h>
#include <chiaki/thread.h>

#include <SDL.h>
#include <winsock2.h>
#include <iphlpapi.h>
#include <libavutil/hwcontext.h>
#include <libswscale/swscale.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define LOG_MASK (CHIAKI_LOG_ALL & ~CHIAKI_LOG_VERBOSE)

static void copy_str(char *dst, size_t n, const char *src)
{
	snprintf(dst, n, "%s", src ? src : "");
}

NAGITO_API int nagito_init(void)
{
	return chiaki_lib_init() == CHIAKI_ERR_SUCCESS ? 0 : -1;
}

/* ---------- discovery ---------- */

typedef struct {
	ChiakiLog log;
	ChiakiDiscoveryService svc;
	NagitoHostsCb cb;
	void *user;
} Discovery;

static void discovery_cb(ChiakiDiscoveryHost *hosts, size_t count, void *user)
{
	Discovery *d = user;
	NagitoHost out[16];
	int n = count > 16 ? 16 : (int)count;
	for(int i = 0; i < n; i++)
	{
		ChiakiDiscoveryHost *h = &hosts[i];
		memset(&out[i], 0, sizeof(out[i]));
		copy_str(out[i].addr, sizeof(out[i].addr), h->host_addr);
		copy_str(out[i].name, sizeof(out[i].name), h->host_name);
		copy_str(out[i].id, sizeof(out[i].id), h->host_id);
		copy_str(out[i].app, sizeof(out[i].app), h->running_app_name);
		out[i].state = h->state == CHIAKI_DISCOVERY_HOST_STATE_READY ? NAGITO_HOST_READY
			: h->state == CHIAKI_DISCOVERY_HOST_STATE_STANDBY ? NAGITO_HOST_STANDBY : NAGITO_HOST_UNKNOWN;
		out[i].ps5 = chiaki_discovery_host_is_ps5(h);
	}
	d->cb(d->user, out, n);
}

NAGITO_API void *nagito_discovery_start(NagitoHostsCb cb, void *user)
{
	Discovery *d = calloc(1, sizeof(Discovery));
	if(!d)
		return NULL;
	d->cb = cb;
	d->user = user;
	chiaki_log_init(&d->log, CHIAKI_LOG_ERROR, chiaki_log_cb_print, NULL);

	ChiakiDiscoveryServiceOptions opt = {0};
	opt.ping_ms = 500;
	opt.hosts_max = 16;
	opt.host_drop_pings = 3;
	opt.cb = discovery_cb;
	opt.cb_user = d;
	// Windows sends 255.255.255.255 out of one adapter only, so also broadcast on every IPv4 subnet.
	struct sockaddr_storage bcast[16];
	size_t nb = 0;
	ULONG size = 32 * 1024;
	IP_ADAPTER_ADDRESSES *aa = malloc(size);
	if(aa && GetAdaptersAddresses(AF_INET, GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER, NULL, aa, &size) == NO_ERROR)
	{
		for(IP_ADAPTER_ADDRESSES *a = aa; a && nb < 16; a = a->Next)
		{
			if(a->OperStatus != IfOperStatusUp || a->IfType == IF_TYPE_SOFTWARE_LOOPBACK)
				continue;
			for(IP_ADAPTER_UNICAST_ADDRESS *u = a->FirstUnicastAddress; u && nb < 16; u = u->Next)
			{
				struct sockaddr_in *ip = (struct sockaddr_in *)u->Address.lpSockaddr;
				if(ip->sin_family != AF_INET || u->OnLinkPrefixLength == 0 || u->OnLinkPrefixLength >= 32)
					continue;
				uint32_t mask = htonl(0xffffffffu << (32 - u->OnLinkPrefixLength));
				struct sockaddr_in b = {0};
				b.sin_family = AF_INET;
				b.sin_addr.s_addr = ip->sin_addr.s_addr | ~mask;
				memset(&bcast[nb], 0, sizeof(bcast[nb]));
				memcpy(&bcast[nb++], &b, sizeof(b));
			}
		}
	}
	free(aa);
	opt.broadcast_addrs = bcast;
	opt.broadcast_num = nb;
	struct sockaddr_in in = {0};
	in.sin_family = AF_INET;
	in.sin_addr.s_addr = 0xffffffff;
	struct sockaddr_storage addr;
	memcpy(&addr, &in, sizeof(in));
	opt.send_addr = &addr;
	opt.send_addr_size = sizeof(in);
	if(chiaki_discovery_service_init(&d->svc, &opt, &d->log) != CHIAKI_ERR_SUCCESS)
	{
		free(d);
		return NULL;
	}
	return d;
}

NAGITO_API void nagito_discovery_stop(void *discovery)
{
	Discovery *d = discovery;
	if(!d)
		return;
	chiaki_discovery_service_fini(&d->svc);
	free(d);
}

NAGITO_API int nagito_wakeup(const char *host, const char regist_key[16], int ps5)
{
	char key[17] = {0};
	memcpy(key, regist_key, 16);
	ChiakiLog log;
	chiaki_log_init(&log, CHIAKI_LOG_ERROR, chiaki_log_cb_print, NULL);
	uint64_t credential = strtoull(key, NULL, 16);
	return chiaki_discovery_wakeup(&log, NULL, host, credential, ps5 != 0) == CHIAKI_ERR_SUCCESS ? 0 : -1;
}

/* ---------- registration (linking a console) ---------- */

typedef struct {
	ChiakiLog log;
	ChiakiRegist regist;
	NagitoRegistCb cb;
	void *user;
	char host[256];
} Regist;

static void regist_cb(ChiakiRegistEvent *e, void *user)
{
	Regist *r = user;
	if(e->type != CHIAKI_REGIST_EVENT_TYPE_FINISHED_SUCCESS || !e->registered_host)
	{
		r->cb(r->user, 0, NULL);
		return;
	}
	ChiakiRegisteredHost *h = e->registered_host;
	NagitoRegistered out = {0};
	copy_str(out.nickname, sizeof(out.nickname), h->server_nickname);
	memcpy(out.mac, h->server_mac, 6);
	memcpy(out.regist_key, h->rp_regist_key, 16);
	memcpy(out.rp_key, h->rp_key, 16);
	r->cb(r->user, 1, &out);
}

NAGITO_API void *nagito_regist_start(const char *host, int ps5, const uint8_t account_id[8], uint32_t pin, NagitoRegistCb cb, void *user)
{
	Regist *r = calloc(1, sizeof(Regist));
	if(!r)
		return NULL;
	r->cb = cb;
	r->user = user;
	copy_str(r->host, sizeof(r->host), host);
	chiaki_log_init(&r->log, LOG_MASK, chiaki_log_cb_print, NULL);
	ChiakiRegistInfo info = {0};
	info.target = ps5 ? CHIAKI_TARGET_PS5_1 : CHIAKI_TARGET_PS4_10;
	info.host = r->host;
	info.broadcast = false;
	memcpy(info.psn_account_id, account_id, 8);
	info.pin = pin;
	if(chiaki_regist_start(&r->regist, &r->log, &info, regist_cb, r) != CHIAKI_ERR_SUCCESS)
	{
		free(r);
		return NULL;
	}
	return r;
}

NAGITO_API void nagito_regist_free(void *regist)
{
	Regist *r = regist;
	if(!r)
		return;
	chiaki_regist_stop(&r->regist);
	chiaki_regist_fini(&r->regist);
	free(r);
}

/* ---------- streaming ---------- */

// DualSense output report, taken from SDL2's testgamecontroller.c (zlib license).
typedef struct {
	Uint8 ucEnableBits1, ucEnableBits2, ucRumbleRight, ucRumbleLeft;
	Uint8 ucHeadphoneVolume, ucSpeakerVolume, ucMicrophoneVolume, ucAudioEnableBits;
	Uint8 ucMicLightMode, ucAudioMuteBits;
	Uint8 rgucRightTriggerEffect[11], rgucLeftTriggerEffect[11];
	Uint8 rgucUnknown1[6], ucEnableBits3, rgucUnknown2[2];
	Uint8 ucLedAnim, ucLedBrightness, ucPadLights, ucLedRed, ucLedGreen, ucLedBlue;
} DS5Effects;

#define MAX_PADS 4
#define MIC_SAMPLES 480 // 10 ms at 48 kHz, what the PS5 expects per Opus packet
#define DUALSENSE_AUDIO_NEEDLE "Wireless Controller" // the DualSense's USB audio device on Windows

typedef struct {
	ChiakiLog log;
	ChiakiSession session;
	ChiakiFfmpegDecoder decoder;
	ChiakiOpusDecoder opus;
	ChiakiOpusEncoder opus_enc;
	NagitoCallbacks cb;
	char host[256];

	struct SwsContext *sws;
	uint8_t *nv12;
	size_t nv12_size;
	int audio_channels, audio_rate;

	ChiakiThread pad_thread;
	volatile int stop;
	ChiakiMutex fx_mutex; // guards everything below that two threads touch
	int rumble_pending, trigger_pending;
	int allow_rumble, allow_triggers, allow_motion, allow_touch, allow_haptics, allow_mic;
	uint32_t button_map[16];
	uint8_t rumble_left, rumble_right;
	ChiakiTriggerEffectsEvent trigger;
	NagitoInput input;

	SDL_AudioDeviceID haptics_dev;
	int16_t *haptics_buf;
	size_t haptics_buf_len;
	int16_t haptics_last[2];

	SDL_AudioDeviceID mic_dev;
	int mic_muted, mic_connected;
	int16_t mic_buf[MIC_SAMPLES * 2];
	int mic_fill;

	ChiakiMutex stats_mutex;
	uint64_t bytes;
	uint64_t stats_at_ms;
	float decode_ms;
} Stream;

static void frame_cb(ChiakiFfmpegDecoder *decoder, void *user)
{
	Stream *s = user;
	int32_t lost;
	ChiakiFfmpegFrame f = chiaki_ffmpeg_decoder_pull_frame(decoder, &lost);
	if(!f.frame)
		return;
	AVFrame *src = f.frame, *sw = NULL;
	if(src->hw_frames_ctx)
	{
		sw = av_frame_alloc();
		if(!sw || av_hwframe_transfer_data(sw, src, 0) < 0)
			goto done;
		src = sw;
	}
	int w = src->width & ~1, h = src->height & ~1;
	size_t need = (size_t)w * h * 3 / 2;
	if(need > s->nv12_size)
	{
		free(s->nv12);
		s->nv12 = malloc(need);
		s->nv12_size = s->nv12 ? need : 0;
	}
	if(!s->nv12)
		goto done;
	uint8_t *y = s->nv12, *uv = s->nv12 + (size_t)w * h;
	if(src->format == AV_PIX_FMT_NV12)
	{
		// The GPU decoder already gives NV12: copy the planes, the shader converts to RGB.
		for(int r = 0; r < h; r++)
			memcpy(y + (size_t)r * w, src->data[0] + (size_t)r * src->linesize[0], w);
		for(int r = 0; r < h / 2; r++)
			memcpy(uv + (size_t)r * w, src->data[1] + (size_t)r * src->linesize[1], w);
	}
	else
	{
		// Software decode (yuv420p) or 10-bit: repack to NV12, no color math here.
		s->sws = sws_getCachedContext(s->sws, src->width, src->height, src->format, w, h, AV_PIX_FMT_NV12, SWS_POINT, NULL, NULL, NULL);
		if(!s->sws)
			goto done;
		uint8_t *dst[4] = {y, uv};
		int stride[4] = {w, w};
		sws_scale(s->sws, (const uint8_t *const *)src->data, src->linesize, 0, src->height, dst, stride);
	}
	int flags = (src->color_range == AVCOL_RANGE_JPEG ? NAGITO_FRAME_FULL_RANGE : 0)
		| (src->colorspace == AVCOL_SPC_BT470BG || src->colorspace == AVCOL_SPC_SMPTE170M ? NAGITO_FRAME_BT601 : 0);
	s->cb.video(s->cb.user, s->nv12, w, h, flags);
done:
	av_frame_free(&sw);
	av_frame_free(&f.frame);
}

// Wraps the decoder's sample callback to measure bitrate and decode time.
static bool video_sample_cb(uint8_t *buf, size_t buf_size, int32_t frames_lost, bool frame_recovered, void *user)
{
	Stream *s = user;
	uint64_t t0 = SDL_GetPerformanceCounter();
	bool ok = chiaki_ffmpeg_decoder_video_sample_cb(buf, buf_size, frames_lost, frame_recovered, &s->decoder);
	float ms = (float)((double)(SDL_GetPerformanceCounter() - t0) * 1000.0 / (double)SDL_GetPerformanceFrequency());
	chiaki_mutex_lock(&s->stats_mutex);
	s->bytes += buf_size;
	s->decode_ms = s->decode_ms == 0.0f ? ms : s->decode_ms * 0.9f + ms * 0.1f;
	chiaki_mutex_unlock(&s->stats_mutex);
	return ok;
}

static void audio_settings_cb(uint32_t channels, uint32_t rate, void *user)
{
	Stream *s = user;
	s->audio_channels = (int)channels;
	s->audio_rate = (int)rate;
}

static void audio_frame_cb(int16_t *buf, size_t frames, void *user)
{
	Stream *s = user;
	s->cb.audio(s->cb.user, buf, frames, s->audio_channels, s->audio_rate);
}

// DualSense haptics arrive as 3 kHz stereo; the pad plays them on channels 3-4 of its 48 kHz USB audio device.
static void haptics_cb(uint8_t *buf, size_t buf_size, void *user)
{
	Stream *s = user;
	const int16_t *in = (const int16_t *)buf;
	size_t frames = buf_size / 4;
	chiaki_mutex_lock(&s->fx_mutex);
	if(!s->haptics_dev || !frames)
		goto out;
	size_t need = frames * 16 * 4;
	if(need > s->haptics_buf_len)
	{
		free(s->haptics_buf);
		s->haptics_buf = malloc(need * sizeof(int16_t));
		s->haptics_buf_len = s->haptics_buf ? need : 0;
		if(!s->haptics_buf)
			goto out;
	}
	int16_t *o = s->haptics_buf;
	for(size_t i = 0; i < frames; i++)
	{
		int16_t l = in[i * 2], r = in[i * 2 + 1];
		for(int k = 1; k <= 16; k++) // linear 16x upsample
		{
			*o++ = 0;
			*o++ = 0;
			*o++ = (int16_t)(s->haptics_last[0] + (l - s->haptics_last[0]) * k / 16);
			*o++ = (int16_t)(s->haptics_last[1] + (r - s->haptics_last[1]) * k / 16);
		}
		s->haptics_last[0] = l;
		s->haptics_last[1] = r;
	}
	// More than 60 ms queued means a network stall; drop it so haptics stay in sync with the picture.
	if(SDL_GetQueuedAudioSize(s->haptics_dev) > 48000 * 4 * 2 * 60 / 1000)
		SDL_ClearQueuedAudio(s->haptics_dev);
	SDL_QueueAudio(s->haptics_dev, s->haptics_buf, (Uint32)(need * sizeof(int16_t)));
out:
	chiaki_mutex_unlock(&s->fx_mutex);
}

static void mic_cb(void *user, Uint8 *data, int len)
{
	Stream *s = user;
	const int16_t *pcm = (const int16_t *)data;
	for(int i = 0; i < len / 2; i++)
	{
		s->mic_buf[s->mic_fill++] = pcm[i];
		if(s->mic_fill == MIC_SAMPLES * 2)
		{
			chiaki_opus_encoder_frame(s->mic_buf, &s->opus_enc);
			s->mic_fill = 0;
		}
	}
}

static void event_cb(ChiakiEvent *e, void *user)
{
	Stream *s = user;
	switch(e->type)
	{
		case CHIAKI_EVENT_CONNECTED:
			s->cb.event(s->cb.user, NAGITO_EV_CONNECTED, 0, "");
			break;
		case CHIAKI_EVENT_QUIT:
		{
			char msg[512];
			snprintf(msg, sizeof(msg), "%s%s%s", chiaki_quit_reason_string(e->quit.reason),
					e->quit.reason_str ? ": " : "", e->quit.reason_str ? e->quit.reason_str : "");
			s->cb.event(s->cb.user, NAGITO_EV_QUIT, chiaki_quit_reason_is_error(e->quit.reason), msg);
			break;
		}
		case CHIAKI_EVENT_LOGIN_PIN_REQUEST:
			s->cb.event(s->cb.user, NAGITO_EV_PIN, e->login_pin_request.pin_incorrect, "");
			break;
		case CHIAKI_EVENT_RUMBLE:
			chiaki_mutex_lock(&s->fx_mutex);
			s->rumble_left = e->rumble.left;
			s->rumble_right = e->rumble.right;
			s->rumble_pending = 1;
			chiaki_mutex_unlock(&s->fx_mutex);
			break;
		case CHIAKI_EVENT_TRIGGER_EFFECTS:
			chiaki_mutex_lock(&s->fx_mutex);
			s->trigger = e->trigger_effects;
			s->trigger_pending = 1;
			chiaki_mutex_unlock(&s->fx_mutex);
			break;
		default:
			break;
	}
}

static const SDL_GameControllerButton PAD_BUTTONS[16] = {
	SDL_CONTROLLER_BUTTON_A, SDL_CONTROLLER_BUTTON_B, SDL_CONTROLLER_BUTTON_X, SDL_CONTROLLER_BUTTON_Y,
	SDL_CONTROLLER_BUTTON_DPAD_LEFT, SDL_CONTROLLER_BUTTON_DPAD_RIGHT, SDL_CONTROLLER_BUTTON_DPAD_UP, SDL_CONTROLLER_BUTTON_DPAD_DOWN,
	SDL_CONTROLLER_BUTTON_LEFTSHOULDER, SDL_CONTROLLER_BUTTON_RIGHTSHOULDER, SDL_CONTROLLER_BUTTON_LEFTSTICK, SDL_CONTROLLER_BUTTON_RIGHTSTICK,
	SDL_CONTROLLER_BUTTON_START, SDL_CONTROLLER_BUTTON_BACK, SDL_CONTROLLER_BUTTON_TOUCHPAD, SDL_CONTROLLER_BUTTON_GUIDE,
};
static const uint32_t PS_BUTTONS[16] = {
	CHIAKI_CONTROLLER_BUTTON_CROSS, CHIAKI_CONTROLLER_BUTTON_MOON, CHIAKI_CONTROLLER_BUTTON_BOX, CHIAKI_CONTROLLER_BUTTON_PYRAMID,
	CHIAKI_CONTROLLER_BUTTON_DPAD_LEFT, CHIAKI_CONTROLLER_BUTTON_DPAD_RIGHT, CHIAKI_CONTROLLER_BUTTON_DPAD_UP, CHIAKI_CONTROLLER_BUTTON_DPAD_DOWN,
	CHIAKI_CONTROLLER_BUTTON_L1, CHIAKI_CONTROLLER_BUTTON_R1, CHIAKI_CONTROLLER_BUTTON_L3, CHIAKI_CONTROLLER_BUTTON_R3,
	CHIAKI_CONTROLLER_BUTTON_OPTIONS, CHIAKI_CONTROLLER_BUTTON_SHARE, CHIAKI_CONTROLLER_BUTTON_TOUCHPAD, CHIAKI_CONTROLLER_BUTTON_PS,
};

static void read_pad(Stream *s, SDL_GameController *c, ChiakiControllerState *st)
{
	for(int i = 0; i < 16; i++)
		if(SDL_GameControllerGetButton(c, PAD_BUTTONS[i]))
			st->buttons |= s->button_map[i];
	st->l2_state = (uint8_t)(SDL_GameControllerGetAxis(c, SDL_CONTROLLER_AXIS_TRIGGERLEFT) >> 7);
	st->r2_state = (uint8_t)(SDL_GameControllerGetAxis(c, SDL_CONTROLLER_AXIS_TRIGGERRIGHT) >> 7);
	st->left_x = SDL_GameControllerGetAxis(c, SDL_CONTROLLER_AXIS_LEFTX);
	st->left_y = SDL_GameControllerGetAxis(c, SDL_CONTROLLER_AXIS_LEFTY);
	st->right_x = SDL_GameControllerGetAxis(c, SDL_CONTROLLER_AXIS_RIGHTX);
	st->right_y = SDL_GameControllerGetAxis(c, SDL_CONTROLLER_AXIS_RIGHTY);
}

static void open_haptics(Stream *s)
{
	if(!s->allow_haptics || s->haptics_dev)
		return;
	SDL_AudioSpec want = {0}, have;
	want.freq = 48000;
	want.format = AUDIO_S16SYS;
	want.channels = 4;
	want.samples = 480;
	for(int i = 0; i < SDL_GetNumAudioDevices(0); i++)
	{
		const char *name = SDL_GetAudioDeviceName(i, 0);
		if(!name || !strstr(name, DUALSENSE_AUDIO_NEEDLE))
			continue;
		SDL_AudioDeviceID dev = SDL_OpenAudioDevice(name, 0, &want, &have, 0);
		if(!dev)
			continue;
		SDL_PauseAudioDevice(dev, 0);
		chiaki_mutex_lock(&s->fx_mutex);
		s->haptics_dev = dev;
		chiaki_mutex_unlock(&s->fx_mutex);
		return;
	}
}

static void toggle_mic(Stream *s)
{
	if(!s->allow_mic)
		return;
	if(!s->mic_dev)
	{
		SDL_AudioSpec want = {0}, have;
		want.freq = 48000;
		want.format = AUDIO_S16SYS;
		want.channels = 2;
		want.samples = MIC_SAMPLES;
		want.callback = mic_cb;
		want.userdata = s;
		// Prefer the DualSense's own microphone, else the Windows default.
		for(int i = 0; i < SDL_GetNumAudioDevices(1) && !s->mic_dev; i++)
		{
			const char *name = SDL_GetAudioDeviceName(i, 1);
			if(name && strstr(name, DUALSENSE_AUDIO_NEEDLE))
				s->mic_dev = SDL_OpenAudioDevice(name, 1, &want, &have, 0);
		}
		if(!s->mic_dev)
			s->mic_dev = SDL_OpenAudioDevice(NULL, 1, &want, &have, 0);
		if(!s->mic_dev)
		{
			s->cb.event(s->cb.user, NAGITO_EV_MIC, 1, SDL_GetError());
			return;
		}
	}
	if(!s->mic_connected)
	{
		chiaki_session_connect_microphone(&s->session);
		s->mic_connected = 1;
	}
	s->mic_muted = !s->mic_muted;
	chiaki_session_toggle_microphone(&s->session, s->mic_muted);
	SDL_PauseAudioDevice(s->mic_dev, s->mic_muted);
	s->cb.event(s->cb.user, NAGITO_EV_MIC, s->mic_muted, "");
}

static void *pad_thread(void *user)
{
	Stream *s = user;
	SDL_SetMainReady();
	SDL_SetHint(SDL_HINT_JOYSTICK_ALLOW_BACKGROUND_EVENTS, "1"); // our window, not SDL's, has focus
	SDL_SetHint(SDL_HINT_JOYSTICK_HIDAPI_PS5_RUMBLE, "1");      // rumble + adaptive triggers over USB and Bluetooth
	if(SDL_Init(SDL_INIT_GAMECONTROLLER | SDL_INIT_AUDIO) < 0)
	{
		s->cb.event(s->cb.user, NAGITO_EV_PADS, -1, SDL_GetError());
		return NULL;
	}
	SDL_GameController *pads[MAX_PADS] = {0};
	ChiakiControllerState last, extra; // extra: touch + motion, which arrive as events
	chiaki_controller_state_set_idle(&last);
	chiaki_controller_state_set_idle(&extra);
	int8_t finger_touch[CHIAKI_CONTROLLER_TOUCHES_MAX] = {-1, -1};
	ChiakiOrientationTracker orient;
	ChiakiAccelNewZero accel_zero, real_accel;
	chiaki_orientation_tracker_init(&orient);
	chiaki_accel_new_zero_set_inactive(&accel_zero, false);
	chiaki_accel_new_zero_set_inactive(&real_accel, true);
	int mic_held = 0;
	s->mic_muted = 1;

	while(!s->stop)
	{
		SDL_Event ev;
		int changed = 0;
		while(SDL_PollEvent(&ev))
		{
			switch(ev.type)
			{
				case SDL_CONTROLLERDEVICEADDED:
					for(int i = 0; i < MAX_PADS; i++)
						if(!pads[i])
						{
							pads[i] = SDL_GameControllerOpen(ev.cdevice.which);
							if(pads[i] && s->allow_motion)
							{
								SDL_GameControllerSetSensorEnabled(pads[i], SDL_SENSOR_ACCEL, SDL_TRUE);
								SDL_GameControllerSetSensorEnabled(pads[i], SDL_SENSOR_GYRO, SDL_TRUE);
							}
							break;
						}
					changed = 1;
					break;
				case SDL_CONTROLLERDEVICEREMOVED:
					for(int i = 0; i < MAX_PADS; i++)
						if(pads[i] && SDL_JoystickInstanceID(SDL_GameControllerGetJoystick(pads[i])) == ev.cdevice.which)
						{
							SDL_GameControllerClose(pads[i]);
							pads[i] = NULL;
						}
					changed = 1;
					break;
				case SDL_CONTROLLERTOUCHPADDOWN:
				case SDL_CONTROLLERTOUCHPADMOTION:
				case SDL_CONTROLLERTOUCHPADUP:
				{
					int f = ev.ctouchpad.finger;
					if(!s->allow_touch || f < 0 || f >= CHIAKI_CONTROLLER_TOUCHES_MAX)
						break;
					// The PS5 touchpad is 1920x1080 points.
					uint16_t x = (uint16_t)(SDL_clamp(ev.ctouchpad.x, 0.0f, 1.0f) * 1919);
					uint16_t y = (uint16_t)(SDL_clamp(ev.ctouchpad.y, 0.0f, 1.0f) * 1079);
					if(ev.type == SDL_CONTROLLERTOUCHPADUP)
					{
						if(finger_touch[f] >= 0)
							chiaki_controller_state_stop_touch(&extra, (uint8_t)finger_touch[f]);
						finger_touch[f] = -1;
					}
					else if(finger_touch[f] < 0)
						finger_touch[f] = chiaki_controller_state_start_touch(&extra, x, y);
					else
						chiaki_controller_state_set_touch_pos(&extra, (uint8_t)finger_touch[f], x, y);
					break;
				}
				case SDL_CONTROLLERSENSORUPDATE:
				{
					if(!s->allow_motion)
						break;
					uint32_t ts = (uint32_t)(ev.csensor.timestamp_us ? ev.csensor.timestamp_us : (Uint64)ev.csensor.timestamp * 1000);
					const float *d = ev.csensor.data;
					if(ev.csensor.sensor == SDL_SENSOR_ACCEL)
					{
						float ax = d[0] / SDL_STANDARD_GRAVITY, ay = d[1] / SDL_STANDARD_GRAVITY, az = d[2] / SDL_STANDARD_GRAVITY;
						chiaki_accel_new_zero_set_active(&real_accel, ax, ay, az, true);
						chiaki_orientation_tracker_update(&orient, extra.gyro_x, extra.gyro_y, extra.gyro_z, ax, ay, az, &accel_zero, false, ts);
					}
					else if(ev.csensor.sensor == SDL_SENSOR_GYRO)
						chiaki_orientation_tracker_update(&orient, d[0], d[1], d[2], extra.accel_x, extra.accel_y, extra.accel_z, &accel_zero, true, ts);
					else
						break;
					chiaki_orientation_tracker_apply_to_controller_state(&orient, &extra);
					break;
				}
				default:
					break;
			}
		}
		if(changed)
		{
			int n = 0;
			const char *name = "";
			for(int i = 0; i < MAX_PADS; i++)
				if(pads[i])
				{
					n++;
					name = SDL_GameControllerName(pads[i]);
					if(SDL_GameControllerGetType(pads[i]) == SDL_CONTROLLER_TYPE_PS5)
						open_haptics(s);
				}
			s->cb.event(s->cb.user, NAGITO_EV_PADS, n, name ? name : "");
		}

		ChiakiControllerState st = extra;
		st.buttons = 0;
		st.l2_state = st.r2_state = 0;
		st.left_x = st.left_y = st.right_x = st.right_y = 0;
		int mic_now = 0;
		for(int i = 0; i < MAX_PADS; i++)
			if(pads[i])
			{
				ChiakiControllerState p = st;
				read_pad(s, pads[i], &p);
				chiaki_controller_state_or(&st, &st, &p);
				mic_now |= SDL_GameControllerGetButton(pads[i], SDL_CONTROLLER_BUTTON_MISC1);
			}
		if(mic_now && !mic_held)
			toggle_mic(s);
		mic_held = mic_now;

		chiaki_mutex_lock(&s->fx_mutex);
		NagitoInput in = s->input;
		int rumble = s->rumble_pending, trig = s->trigger_pending;
		uint8_t rl = s->rumble_left, rr = s->rumble_right;
		ChiakiTriggerEffectsEvent te = s->trigger;
		s->rumble_pending = s->trigger_pending = 0;
		chiaki_mutex_unlock(&s->fx_mutex);

		ChiakiControllerState kb = st; // keyboard/mouse, merged like another pad
		kb.buttons = in.buttons;
		kb.l2_state = in.l2;
		kb.r2_state = in.r2;
		kb.left_x = in.lx;
		kb.left_y = in.ly;
		kb.right_x = in.rx;
		kb.right_y = in.ry;
		chiaki_controller_state_or(&st, &st, &kb);
		if(!chiaki_controller_state_equals(&st, &last))
		{
			chiaki_session_set_controller_state(&s->session, &st);
			last = st;
		}

		for(int i = 0; i < MAX_PADS; i++)
		{
			if(!pads[i])
				continue;
			if(rumble && s->allow_rumble)
				SDL_GameControllerRumble(pads[i], (Uint16)(rl << 8), (Uint16)(rr << 8), 5000);
			if(trig && s->allow_triggers && SDL_GameControllerGetType(pads[i]) == SDL_CONTROLLER_TYPE_PS5)
			{
				DS5Effects fx;
				SDL_zero(fx);
				fx.ucEnableBits1 = 0x04 | 0x08;
				fx.rgucLeftTriggerEffect[0] = te.type_left;
				memcpy(fx.rgucLeftTriggerEffect + 1, te.left, 10);
				fx.rgucRightTriggerEffect[0] = te.type_right;
				memcpy(fx.rgucRightTriggerEffect + 1, te.right, 10);
				SDL_GameControllerSendEffect(pads[i], &fx, sizeof(fx));
			}
		}
		SDL_Delay(2);
	}
	if(s->mic_dev)
		SDL_CloseAudioDevice(s->mic_dev); // waits for mic_cb to finish
	chiaki_mutex_lock(&s->fx_mutex);
	SDL_AudioDeviceID hd = s->haptics_dev;
	s->haptics_dev = 0;
	chiaki_mutex_unlock(&s->fx_mutex);
	if(hd)
		SDL_CloseAudioDevice(hd);
	for(int i = 0; i < MAX_PADS; i++)
		if(pads[i])
			SDL_GameControllerClose(pads[i]);
	SDL_QuitSubSystem(SDL_INIT_GAMECONTROLLER | SDL_INIT_AUDIO);
	return NULL;
}

NAGITO_API void *nagito_stream_start(const NagitoStreamConfig *cfg, const NagitoCallbacks *cb, char *err, size_t err_size)
{
	Stream *s = calloc(1, sizeof(Stream));
	if(!s)
		return NULL;
	s->cb = *cb;
	s->allow_rumble = cfg->rumble;
	s->allow_triggers = cfg->triggers;
	s->allow_motion = cfg->motion;
	s->allow_touch = cfg->touchpad;
	s->allow_haptics = cfg->haptics;
	s->allow_mic = cfg->mic;
	for(int i = 0; i < 16; i++)
		s->button_map[i] = cfg->button_map[i] ? cfg->button_map[i] : PS_BUTTONS[i];
	copy_str(s->host, sizeof(s->host), cfg->host);
	chiaki_log_init(&s->log, LOG_MASK, chiaki_log_cb_print, NULL);
	chiaki_mutex_init(&s->fx_mutex, false);
	chiaki_mutex_init(&s->stats_mutex, false);
	s->stats_at_ms = SDL_GetTicks64();

	ChiakiCodec codec = cfg->ps5 && cfg->hevc ? CHIAKI_CODEC_H265 : CHIAKI_CODEC_H264;
	if(chiaki_ffmpeg_decoder_init(&s->decoder, &s->log, codec, (unsigned)cfg->fps, "d3d11va", NULL, frame_cb, s) != CHIAKI_ERR_SUCCESS
		&& chiaki_ffmpeg_decoder_init(&s->decoder, &s->log, codec, (unsigned)cfg->fps, NULL, NULL, frame_cb, s) != CHIAKI_ERR_SUCCESS)
	{
		copy_str(err, err_size, "Could not start the video decoder");
		goto fail_mutex;
	}
	chiaki_opus_decoder_init(&s->opus, &s->log);
	chiaki_opus_decoder_set_cb(&s->opus, audio_settings_cb, audio_frame_cb, s);
	chiaki_opus_encoder_init(&s->opus_enc, &s->log);

	ChiakiConnectInfo ci = {0};
	ci.ps5 = cfg->ps5 != 0;
	ci.host = s->host;
	memcpy(ci.regist_key, cfg->regist_key, sizeof(ci.regist_key));
	memcpy(ci.morning, cfg->rp_key, sizeof(ci.morning));
	ci.video_profile.width = (unsigned)cfg->width;
	ci.video_profile.height = (unsigned)cfg->height;
	ci.video_profile.max_fps = (unsigned)cfg->fps;
	ci.video_profile.bitrate = (unsigned)cfg->bitrate_kbps;
	ci.video_profile.codec = codec;
	ci.video_profile_auto_downgrade = true;
	ci.enable_dualsense = true;
	ci.packet_loss_max = 0.05;
	ci.enable_idr_on_fec_failure = true;
	memcpy(ci.psn_account_id, cfg->account_id, sizeof(ci.psn_account_id));

	ChiakiErrorCode e = chiaki_session_init(&s->session, &ci, &s->log);
	if(e != CHIAKI_ERR_SUCCESS)
	{
		snprintf(err, err_size, "Could not set up the session: %s", chiaki_error_string(e));
		goto fail_decoders;
	}
	ChiakiAudioSink sink;
	chiaki_opus_decoder_get_sink(&s->opus, &sink);
	chiaki_session_set_audio_sink(&s->session, &sink);
	if(cfg->haptics)
	{
		ChiakiAudioSink haptics = {0};
		haptics.user = s;
		haptics.frame_cb = haptics_cb;
		chiaki_session_set_haptics_sink(&s->session, &haptics);
	}
	ChiakiAudioHeader mic_header;
	chiaki_audio_header_set(&mic_header, 2, 16, MIC_SAMPLES * 100, MIC_SAMPLES);
	chiaki_opus_encoder_header(&mic_header, &s->opus_enc, &s->session);
	chiaki_session_set_video_sample_cb(&s->session, video_sample_cb, s);
	chiaki_session_set_event_cb(&s->session, event_cb, s);
	e = chiaki_session_start(&s->session);
	if(e != CHIAKI_ERR_SUCCESS)
	{
		snprintf(err, err_size, "Could not connect: %s", chiaki_error_string(e));
		chiaki_session_fini(&s->session);
		goto fail_decoders;
	}
	chiaki_thread_create(&s->pad_thread, pad_thread, s);
	return s;

fail_decoders:
	chiaki_opus_encoder_fini(&s->opus_enc);
	chiaki_opus_decoder_fini(&s->opus);
	chiaki_ffmpeg_decoder_fini(&s->decoder);
fail_mutex:
	chiaki_mutex_fini(&s->stats_mutex);
	chiaki_mutex_fini(&s->fx_mutex);
	free(s);
	return NULL;
}

NAGITO_API void nagito_stream_set_pin(void *stream, const char *pin)
{
	Stream *s = stream;
	if(s && pin)
		chiaki_session_set_login_pin(&s->session, (const uint8_t *)pin, strlen(pin));
}

NAGITO_API void nagito_stream_set_input(void *stream, const NagitoInput *input)
{
	Stream *s = stream;
	if(!s || !input)
		return;
	chiaki_mutex_lock(&s->fx_mutex);
	s->input = *input;
	chiaki_mutex_unlock(&s->fx_mutex);
}

NAGITO_API void nagito_stream_stats(void *stream, NagitoStats *out)
{
	Stream *s = stream;
	memset(out, 0, sizeof(*out));
	if(!s)
		return;
	uint64_t received = 0, lost = 0;
	chiaki_packet_stats_get(&s->session.stream_connection.packet_stats, false, &received, &lost);
	out->loss = received + lost ? (float)lost / (float)(received + lost) : 0.0f;
	out->rtt_ms = (float)s->session.rtt_us / 1000.0f;
	uint64_t now = SDL_GetTicks64();
	chiaki_mutex_lock(&s->stats_mutex);
	uint64_t dt = now - s->stats_at_ms;
	if(dt > 0)
		out->bitrate_mbps = (float)((double)s->bytes * 8.0 / 1000.0 / (double)dt);
	out->decode_ms = s->decode_ms;
	s->bytes = 0;
	s->stats_at_ms = now;
	chiaki_mutex_unlock(&s->stats_mutex);
}

NAGITO_API void nagito_stream_stop(void *stream)
{
	Stream *s = stream;
	if(!s)
		return;
	s->stop = 1;
	chiaki_thread_join(&s->pad_thread, NULL);
	chiaki_session_stop(&s->session);
	chiaki_session_join(&s->session);
	chiaki_session_fini(&s->session);
	chiaki_opus_encoder_fini(&s->opus_enc);
	chiaki_opus_decoder_fini(&s->opus);
	chiaki_ffmpeg_decoder_fini(&s->decoder);
	chiaki_mutex_fini(&s->stats_mutex);
	chiaki_mutex_fini(&s->fx_mutex);
	sws_freeContext(s->sws);
	free(s->haptics_buf);
	free(s->nv12);
	free(s);
}
