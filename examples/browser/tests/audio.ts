export function receivedTone(
	samples: readonly number[],
	sampleRate: number,
	wanted: number,
	other: number,
): boolean {
	if (
		samples.length < 128 ||
		samples.length > 48000 ||
		!Number.isFinite(sampleRate) ||
		sampleRate < 8000 ||
		sampleRate > 48000 ||
		![wanted, other].every(
			(frequency: number): boolean =>
				Number.isFinite(frequency) &&
				frequency > 0 &&
				frequency < sampleRate / 2,
		) ||
		wanted === other ||
		!samples.every(
			(sample: number): boolean =>
				Number.isFinite(sample) && Math.abs(sample) <= 1,
		)
	)
		return false;
	function amplitude(frequency: number): number {
		let sine = 0;
		let cosine = 0;
		for (const [index, sample] of samples.entries()) {
			const phase = (2 * Math.PI * frequency * index) / sampleRate;
			sine += sample * Math.sin(phase);
			cosine += sample * Math.cos(phase);
		}
		return (2 * Math.hypot(sine, cosine)) / samples.length;
	}
	const expected = amplitude(wanted);
	return expected >= 0.01 && amplitude(other) < expected * 0.3;
}
