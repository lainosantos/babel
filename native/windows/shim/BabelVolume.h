// Babel control-only volume protocol v1; MIT. Each topology endpoint owns its
// state. These values are intentionally never applied to the cable's PCM: the
// application mirrors the Speaker control to its selected physical output.
#pragma once

struct BabelVolumeState final {
    static const ULONG Channels = 2;
    volatile LONG levels[Channels] = {0, 0};
    volatile LONG muted[Channels] = {0, 0};

    LONG MixerVolumeRead(ULONG, ULONG channel) {
        return channel < Channels ? InterlockedCompareExchange(&levels[channel], 0, 0) : 0;
    }
    void MixerVolumeWrite(ULONG, ULONG channel, LONG value) {
        if (channel < Channels) InterlockedExchange(&levels[channel], value);
    }
    BOOL MixerMuteRead(ULONG, ULONG channel) {
        return channel < Channels ? InterlockedCompareExchange(&muted[channel], 0, 0) != 0 : FALSE;
    }
    void MixerMuteWrite(ULONG, ULONG channel, BOOL value) {
        if (channel < Channels) InterlockedExchange(&muted[channel], value ? 1 : 0);
    }
};
