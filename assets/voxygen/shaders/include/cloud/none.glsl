#include <lod.glsl>
#include <sky.glsl>

vec3 get_cloud_color(vec3 surf_color, vec3 dir, vec3 origin, float max_dist, float quality) {
    // Underwater light attenuation
    surf_color = water_diffuse(surf_color, dir, max_dist);

    vec3 sky_light = get_sky_light(dir, false, 0.0);
    if (max_dist < DIST_CAP) {
        surf_color = mix(sky_light, surf_color, 1.0 / exp(max_dist / 5000.0));
    }
    
    // This is rubbish... but fast
    float cloud_alt = cloud_avg_alt();
    if (dir.z * (cloud_alt - origin.z) > 0.0) {
        float dist = (cloud_alt - origin.z) / dir.z;
        if (dist < max_dist) {
            vec2 cloud_intersect = origin.xy + focus_off.xy + dir.xy * dist;
            surf_color = mix(
                surf_color,
                vec3(1 + noise_3d(vec3(cloud_intersect * 0.0001, time_of_day.x * 0.001)) * 2.0) * sky_light,
                min(cloud_tendency_at(cloud_intersect) * 30, 1)
            );
        }
    }

    return surf_color;
}
